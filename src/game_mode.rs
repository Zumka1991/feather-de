//! Game Mode & Background Resource Optimizer.
//!
//! Keeps foreground games smooth by throttling background `.scope` units in `app.slice`,
//! and conversely caps unfocused/minimized games (`cpu.max` + `cpu.idle` + `cpu.weight`)
//! when `Alt+Tab`bing back to the desktop so DXVK/VKD3D stops saturating the GPU at 100%
//! while keeping TCP/UDP game heartbeats and audio streams alive.
//!
//! Zero work on the compositor event loop: no `/proc`, `/sys/fs/cgroup`, X11, or `pactl`
//! calls ever run on the main thread. The worker thread spawns lazily on the first game
//! window and sleeps in `Receiver::recv` (`0 %` CPU in idle).

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, OnceLock,
    },
    time::{Duration, Instant},
};

static GAME_MODE_ENABLED: AtomicBool = AtomicBool::new(true);

pub fn game_mode_enabled() -> bool {
    GAME_MODE_ENABLED.load(Ordering::Relaxed)
}

pub fn set_game_mode_enabled(on: bool) {
    GAME_MODE_ENABLED.store(on, Ordering::Relaxed);
}

/// CPU quota (`quota_us period_us`) for an unfocused/minimized game: ~8.3% of one core
/// using a 12 ms micro-period (`1000 µs / 12000 µs`).
///
/// NOTE: Linux kernel CFS bandwidth control enforces `quota >= 1000 µs` (`min_cfs_quota_period`);
/// smaller quotas like `"750 5000"` fail with `EINVAL` and leave the scope uncapped.
/// With `1000 12000`, TCP/UDP game server heartbeats stay alive while DXVK/VKD3D GPU submission
/// drops from 100% down to near-zero.
const UNFOCUSED_GAME_CPU_MAX: &str = "1000 12000";

/// CPU quota for non-audio, non-VPN background apps while a game is actively focused:
/// 35% of one core max (`3500 µs / 10000 µs` 10 ms micro-period) + `SCHED_IDLE` (`cpu.idle = 1`).
const BACKGROUND_APP_CPU_MAX: &str = "3500 10000";

const UNLIMITED_CPU_MAX: &str = "max 100000";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemoryLowTier {
    None,
    Foreground,
    FocusedGame,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeRole {
    /// Default desktop scheduling (`cpu.max = max`, `cpu.idle = 0`, `cpu.weight = 100`)
    Normal,
    /// Focused desktop app (`cpu.weight = 500`, soft `memory.low` protection)
    ForegroundDesktop,
    /// Actively focused game (`cpu.max = max`, `cpu.idle = 0`, `cpu.weight = 1000`, soft `memory.low`)
    FocusedGame,
    /// App Nap: desktop app whose windows are all minimized, off-space, or occluded
    /// (`cpu.max = max` so idle-core work finishes at full speed, `cpu.idle = 1`, `cpu.weight = 10`)
    BackgroundHiddenApp,
    /// Unfocused or minimized game (`cpu.max = 1000 12000`, `cpu.idle = 1`, `cpu.weight = 1`)
    BackgroundGame,
    /// Background non-audio/non-VPN app while a game is focused (`cpu.max = 3500 10000`, `cpu.idle = 1`)
    BackgroundAppDuringGame,
}

impl ScopeRole {
    fn params(self) -> (&'static str, &'static str, &'static str, MemoryLowTier) {
        // Returns (cpu.max, cpu.idle, cpu.weight, memory.low tier)
        match self {
            Self::Normal => (UNLIMITED_CPU_MAX, "0", "100", MemoryLowTier::None),
            Self::ForegroundDesktop => (UNLIMITED_CPU_MAX, "0", "500", MemoryLowTier::Foreground),
            Self::FocusedGame => (UNLIMITED_CPU_MAX, "0", "1000", MemoryLowTier::FocusedGame),
            // On Linux cgroup v2, `cpu.idle = 1` pins `cpu.weight` to 1; fallback weight is used
            // only on older kernels that do not expose `cpu.idle`.
            Self::BackgroundHiddenApp => (UNLIMITED_CPU_MAX, "1", "10", MemoryLowTier::None),
            Self::BackgroundGame => (UNFOCUSED_GAME_CPU_MAX, "1", "1", MemoryLowTier::None),
            Self::BackgroundAppDuringGame => (BACKGROUND_APP_CPU_MAX, "1", "5", MemoryLowTier::None),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSample {
    pub id: u64,
    pub wl_client_pid: Option<u32>,
    pub xwayland: bool,
    pub fullscreen: bool,
    pub minimized: bool,
    pub visible: bool,
    /// True when drawn in the last rendered scene (visible and NOT occluded by opaque windows)
    pub shown: bool,
    pub focused: bool,
    pub is_game: bool,
    pub app_id: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub enabled: bool,
    pub x11_display: Option<String>,
    pub satellite_pid: Option<u32>,
    pub windows: Vec<WindowSample>,
}

enum WorkerMsg {
    Apply(Snapshot),
    Shutdown(mpsc::SyncSender<()>),
}

#[derive(Default)]
pub struct GameMode {
    had_active_policy: bool,
    last_sent: Option<Snapshot>,
}

fn worker_sender() -> Option<&'static mpsc::Sender<WorkerMsg>> {
    static WORKER: OnceLock<Option<mpsc::Sender<WorkerMsg>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<WorkerMsg>();
            std::thread::Builder::new()
                .name("feather-gamemode".into())
                .stack_size(256 * 1024)
                .spawn(move || worker_loop(rx))
                .ok()?;
            Some(tx)
        })
        .as_ref()
}

impl GameMode {
    pub fn sync(&mut self, snapshot: Snapshot) {
        let any_game = snapshot.enabled && snapshot.windows.iter().any(|w| w.is_game);
        let any_hidden = snapshot.enabled && snapshot.windows.iter().any(|w| !w.shown && !w.focused);
        let active = any_game || any_hidden;
        if !active && !self.had_active_policy {
            return;
        }
        if self.last_sent.as_ref() == Some(&snapshot) {
            return;
        }
        self.had_active_policy = active;
        self.last_sent = Some(snapshot.clone());
        if let Some(tx) = worker_sender() {
            let _ = tx.send(WorkerMsg::Apply(snapshot));
        }
    }

    pub fn shutdown(&mut self) {
        if !self.had_active_policy && self.last_sent.is_none() {
            return;
        }
        self.had_active_policy = false;
        self.last_sent = None;
        if let Some(tx) = worker_sender() {
            let (done_tx, done_rx) = mpsc::sync_channel(1);
            if tx.send(WorkerMsg::Shutdown(done_tx)).is_ok() {
                let _ = done_rx.recv_timeout(Duration::from_millis(250));
            }
        }
    }
}

/// One-shot startup boost for the compositor main thread and Mesa GPU submission threads
/// (`:cs0`, `:gl0`, `:gdrv0`) via `org.freedesktop.RealtimeKit1::MakeThreadHighPriority`.
/// Uses high nice (`-11`, clamped to `MinNiceLevel`) instead of `SCHED_RR` so cold shader
/// compilations can never trigger `RLIMIT_RTTIME` / `SIGKILL`, while giving the compositor
/// ~11× EEVDF weight over normal apps and instant preemption over `cpu.idle = 1` scopes.
///
/// Spawns a tiny short-lived thread so the event loop never blocks on D-Bus; the thread exits
/// immediately after the RPC completes (0 persistent threads, 0 % idle CPU).
pub fn boost_compositor_priority() {
    let main_tid = unsafe { libc::syscall(libc::SYS_gettid) as u64 };
    if main_tid == 0 {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("feather-rtkit".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            let Ok(conn) = zbus::blocking::Connection::system() else {
                return;
            };
            let Ok(proxy) = zbus::blocking::Proxy::new(
                &conn,
                "org.freedesktop.RealtimeKit1",
                "/org/freedesktop/RealtimeKit1",
                "org.freedesktop.RealtimeKit1",
            ) else {
                return;
            };
            let min_nice: i32 = proxy.get_property("MinNiceLevel").unwrap_or(-11);
            let target_nice = (-11i32).max(min_nice).min(-1);

            let mut tids = vec![(main_tid, "main".to_string())];
            if let Ok(entries) = fs::read_dir("/proc/self/task") {
                for entry in entries.flatten() {
                    let Some(tid) = entry.file_name().to_str().and_then(|s| s.parse::<u64>().ok()) else {
                        continue;
                    };
                    if tid == main_tid {
                        continue;
                    }
                    let comm = fs::read_to_string(entry.path().join("comm"))
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    if comm.ends_with(":cs0") || comm.ends_with(":gl0") || comm.ends_with(":gdrv0") {
                        tids.push((tid, comm));
                    }
                }
            }

            let mut boosted = Vec::new();
            for (tid, label) in tids {
                if proxy
                    .call::<_, _, ()>("MakeThreadHighPriority", &(tid, target_nice))
                    .is_ok()
                {
                    boosted.push(label);
                }
            }
            if !boosted.is_empty() {
                tracing::info!(
                    "rtkit: compositor priority nice={target_nice} applied to [{}]",
                    boosted.join(", ")
                );
            }
        });
}

/// Game store / launcher main windows have `Categories=Game;` in their `.desktop` files,
/// but should not trigger Game Mode or get CPU-capped while downloading games in the background.
pub fn is_launcher_app_id(app_id: &str) -> bool {
    let lower = app_id.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "steam"
            | "com.valvesoftware.steam"
            | "heroic"
            | "com.heroicgameslauncher.hgl"
            | "lutris"
            | "net.lutris.lutris"
            | "bottles"
            | "com.usebottles.bottles"
            | "itch"
            | "io.itch.itch"
            | "minigalaxy"
            | "io.github.sharkwouter.minigalaxy"
    )
}

pub fn is_non_game_app_id(app_id: &str) -> bool {
    let lower = app_id.trim().to_ascii_lowercase();
    if lower.starts_with("builtin.") || lower.starts_with("feather-") {
        return true;
    }
    const NON_GAMES: &[&str] = &[
        "chrome",
        "chromium",
        "firefox",
        "brave",
        "vivaldi",
        "zen",
        "microsoft-edge",
        "librewolf",
        "waterfox",
        "epiphany",
        "mpv",
        "vlc",
        "celluloid",
        "totem",
        "haruna",
        "smplayer",
        "kodi",
        "obs",
        "com.obsproject.studio",
        "telegram",
        "discord",
        "vesktop",
        "slack",
        "element",
        "signal",
        "thunderbird",
        "antigravity",
        "code",
        "vscodium",
        "zed",
        "cursor",
        "jetbrains",
        "konsole",
        "kitty",
        "alacritty",
        "wezterm",
        "foot",
        "ghostty",
        "gnome-terminal",
        "dolphin",
        "nautilus",
        "thunar",
        "nemo",
        "libreoffice",
        "soffice",
        "gimp",
        "inkscape",
        "krita",
        "blender",
        "audacity",
        "kdenlive",
    ];
    NON_GAMES.iter().any(|p| lower == *p || lower.contains(p))
}

/// Wine/Proton store launchers share generic `steam_app_default` `WM_CLASS`, but when not
/// fullscreen they are game downloaders/launchers rather than 3D games — they must not trigger
/// hard Game Mode throttling while patching or updating games in the background.
pub fn is_launcher_title(title: &str) -> bool {
    let lower = title.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "battle.net"
            | "blizzard battle.net"
            | "battle.net update agent"
            | "ubisoft connect"
            | "ea"
            | "ea app"
            | "epic games launcher"
            | "rockstar games launcher"
            | "gog galaxy"
            | "riot client"
            | "wargaming.net game center"
            | "vk play game center"
    )
}

#[allow(clippy::too_many_arguments)]
pub fn classify_game_window(
    app_id: &str,
    title: &str,
    fullscreen: bool,
    xwayland: bool,
    desktop_is_game: bool,
    pointer_constrained: bool,
    is_builtin: bool,
    larvis: bool,
    pip: bool,
    auxiliary: bool,
) -> bool {
    if is_builtin || larvis || pip || auxiliary {
        return false;
    }
    if (is_launcher_app_id(app_id) || is_launcher_title(title)) && !fullscreen {
        return false;
    }
    if is_non_game_app_id(app_id) {
        return false;
    }
    let lower = app_id.trim().to_ascii_lowercase();
    if lower.starts_with("steam_app_")
        || lower.ends_with(".exe")
        || lower.contains("gamescope")
        || lower.contains("minecraft")
        || lower.contains("retroarch")
        || lower.contains("ryujinx")
        || lower.contains("yuzu")
        || lower.contains("rpcs3")
        || lower.contains("pcsx2")
        || lower.contains("dolphin-emu")
        || lower.contains("duckstation")
        || lower.contains("cemu")
    {
        return true;
    }
    if desktop_is_game && !is_launcher_app_id(app_id) {
        return true;
    }
    if fullscreen && (xwayland || pointer_constrained) {
        return true;
    }
    let title_lower = title.to_ascii_lowercase();
    fullscreen && title_lower.contains("world of warcraft")
}

/// Pure policy planner: given all active `.scope` names in `app.slice` and which scopes hold
/// focused games, unfocused games, the focused desktop app, currently shown desktop windows,
/// mapped windowed apps (including hidden/occluded ones), and exempt streams (active audio / VPN),
/// computes the target [`ScopeRole`] for every `.scope`.
pub fn plan_scope_roles(
    all_scopes: &[String],
    focused_game_scopes: &HashSet<String>,
    unfocused_game_scopes: &HashSet<String>,
    focused_desktop_scopes: &HashSet<String>,
    visible_desktop_scopes: &HashSet<String>,
    windowed_scopes: &HashSet<String>,
    exempt_scopes: &HashSet<String>,
) -> HashMap<String, ScopeRole> {
    let mut out = HashMap::with_capacity(all_scopes.len());
    let has_focused_game = !focused_game_scopes.is_empty();
    let has_any_game = has_focused_game || !unfocused_game_scopes.is_empty();
    let has_hidden_windowed = windowed_scopes.iter().any(|s| {
        !focused_desktop_scopes.contains(s)
            && !visible_desktop_scopes.contains(s)
            && !focused_game_scopes.contains(s)
            && !unfocused_game_scopes.contains(s)
    });
    if !has_any_game && !has_hidden_windowed {
        for s in all_scopes {
            out.insert(s.clone(), ScopeRole::Normal);
        }
        return out;
    }

    for s in all_scopes {
        let role = if focused_game_scopes.contains(s) {
            // A scope shared by a launcher (Heroic/Steam) and a focused game always gets
            // full game priority.
            ScopeRole::FocusedGame
        } else if unfocused_game_scopes.contains(s) && !focused_desktop_scopes.contains(s) {
            ScopeRole::BackgroundGame
        } else if has_focused_game {
            if exempt_scopes.contains(s) {
                ScopeRole::Normal
            } else {
                ScopeRole::BackgroundAppDuringGame
            }
        } else if focused_desktop_scopes.contains(s) {
            ScopeRole::ForegroundDesktop
        } else if exempt_scopes.contains(s) || visible_desktop_scopes.contains(s) {
            ScopeRole::Normal
        } else if windowed_scopes.contains(s) {
            // App Nap: every window in this scope is minimized, off-space, or occluded behind
            // opaque windows — keep CPU uncapped on idle cores, but yield immediately (`cpu.idle=1`).
            ScopeRole::BackgroundHiddenApp
        } else {
            ScopeRole::Normal
        };
        out.insert(s.clone(), role);
    }
    out
}

// ---------------------------------------------------------------------------
// Background worker implementation (runs exclusively off the event loop)
// ---------------------------------------------------------------------------

fn auto_muted_state_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("feather-de/gamemode-muted"))
}

struct WorkerState {
    touched_scopes: HashMap<PathBuf, ScopeRole>,
    touched_pids: HashSet<i32>,
    auto_muted_indices: HashSet<u32>,
    auto_muted_keys: HashSet<String>,
    subtree_cpu_enabled: bool,
}

impl WorkerState {
    fn new() -> Self {
        let auto_muted_keys = auto_muted_state_file()
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|t| {
                t.lines()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            touched_scopes: HashMap::new(),
            touched_pids: HashSet::new(),
            auto_muted_indices: HashSet::new(),
            auto_muted_keys,
            subtree_cpu_enabled: false,
        }
    }

    fn persist_muted_keys(&self) {
        let Some(path) = auto_muted_state_file() else {
            return;
        };
        if self.auto_muted_keys.is_empty() {
            let _ = fs::remove_file(path);
        } else {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let mut body = String::new();
            for k in &self.auto_muted_keys {
                body.push_str(k);
                body.push('\n');
            }
            let _ = fs::write(path, body);
        }
    }

    fn restore_all(&mut self) {
        for (path, role) in self.touched_scopes.drain() {
            if role != ScopeRole::Normal && path.is_dir() {
                write_scope_role(&path, ScopeRole::Normal);
            }
        }
        for pid in self.touched_pids.drain() {
            set_pid_idle_priority(pid, false);
        }
        if !self.auto_muted_indices.is_empty() || !self.auto_muted_keys.is_empty() {
            let snap = query_audio_snapshot(unsafe { libc::getuid() });
            for si in &snap.sink_inputs {
                if si.muted
                    && (self.auto_muted_indices.contains(&si.index)
                        || (!si.restore_key.is_empty()
                            && self.auto_muted_keys.contains(&si.restore_key)))
                {
                    set_sink_input_mute(si.index, false);
                    self.auto_muted_keys.remove(&si.restore_key);
                }
            }
            self.auto_muted_indices.clear();
            self.persist_muted_keys();
        }
    }
}

fn worker_loop(rx: mpsc::Receiver<WorkerMsg>) {
    let mut state = WorkerState::new();
    let mut follow_up: Option<Snapshot> = None;
    loop {
        let received = match follow_up.take() {
            Some(prev) => match rx.recv_timeout(Duration::from_millis(1500)) {
                Ok(msg) => Ok(msg),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // One-shot follow-up catches game audio streams opened slightly after mapping;
                    // after this single pass `follow_up` stays `None` and we block infinitely (`0 %` CPU).
                    apply_snapshot(&mut state, &prev);
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(()),
            },
            None => rx.recv().map_err(|_| ()),
        };
        let Ok(msg) = received else {
            break;
        };
        match msg {
            WorkerMsg::Apply(mut latest) => {
                // Coalesce rapid focus transitions if more snapshots arrived while waking up
                while let Ok(next) = rx.try_recv() {
                    match next {
                        WorkerMsg::Apply(s) => latest = s,
                        WorkerMsg::Shutdown(done) => {
                            state.restore_all();
                            let _ = done.send(());
                            return;
                        }
                    }
                }
                apply_snapshot(&mut state, &latest);
                if latest.enabled && latest.windows.iter().any(|w| w.is_game) {
                    follow_up = Some(latest);
                }
            }
            WorkerMsg::Shutdown(done) => {
                state.restore_all();
                let _ = done.send(());
                return;
            }
        }
    }
    state.restore_all();
}

fn user_app_slice_dir() -> Option<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let path = PathBuf::from(format!(
        "/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/app.slice"
    ));
    path.is_dir().then_some(path)
}

fn ensure_subtree_cpu_controller(app_slice: &Path, cached: &mut bool) -> bool {
    if *cached {
        return true;
    }
    let ctrl_path = app_slice.join("cgroup.subtree_control");
    if let Ok(current) = fs::read_to_string(&ctrl_path) {
        if current.split_whitespace().any(|c| c == "cpu") {
            *cached = true;
            return true;
        }
    }
    if fs::write(&ctrl_path, "+cpu").is_ok() {
        *cached = true;
        return true;
    }
    false
}

fn total_ram_bytes() -> u64 {
    static TOTAL: OnceLock<u64> = OnceLock::new();
    *TOTAL.get_or_init(|| {
        fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|txt| {
                txt.lines().find_map(|line| {
                    let rest = line.strip_prefix("MemTotal:")?;
                    let kib = rest.split_whitespace().next()?.parse::<u64>().ok()?;
                    Some(kib.saturating_mul(1024))
                })
            })
            .unwrap_or(8 * 1024 * 1024 * 1024)
    })
}

fn memory_low_bytes_for(tier: MemoryLowTier) -> u64 {
    let ram = total_ram_bytes();
    const MIB: u64 = 1024 * 1024;
    match tier {
        MemoryLowTier::None => 0,
        MemoryLowTier::Foreground => (ram / 8).clamp(512 * MIB, 2048 * MIB),
        MemoryLowTier::FocusedGame => (ram / 4).clamp(1024 * MIB, 4096 * MIB),
    }
}

fn write_cgroup_param(path: &Path, value: &str, optional: bool) {
    if let Err(err) = fs::write(path, value) {
        let ignore = optional
            && matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            );
        if !ignore {
            tracing::warn!(
                "game_mode: failed to write {} = {:?}: {err}",
                path.display(),
                value
            );
        }
    }
}

fn write_scope_role(scope_dir: &Path, role: ScopeRole) {
    let (cpu_max, cpu_idle, cpu_weight, mem_tier) = role.params();
    let mem_low = memory_low_bytes_for(mem_tier).to_string();
    let mem_path = scope_dir.join("memory.low");
    if cpu_idle == "0" {
        // Restore idle & quota before raising weight so the kernel accepts weight immediately
        write_cgroup_param(&scope_dir.join("cpu.idle"), cpu_idle, true);
        write_cgroup_param(&scope_dir.join("cpu.max"), cpu_max, false);
        write_cgroup_param(&scope_dir.join("cpu.weight"), cpu_weight, false);
        if mem_path.exists() {
            write_cgroup_param(&mem_path, &mem_low, true);
        }
    } else {
        if mem_path.exists() {
            write_cgroup_param(&mem_path, &mem_low, true);
        }
        // Setting `cpu.idle = 1` automatically pins `cpu.weight` to 1 in Linux cgroup v2 (and
        // writing a non-1 `cpu.weight` while `cpu.idle == 1` returns EINVAL). Only write
        // `cpu.weight` as a fallback when `cpu.idle` is not supported by the kernel.
        if fs::write(scope_dir.join("cpu.idle"), cpu_idle).is_err() {
            write_cgroup_param(&scope_dir.join("cpu.weight"), cpu_weight, false);
        }
        write_cgroup_param(&scope_dir.join("cpu.max"), cpu_max, false);
    }
}

/// Parses `/proc/<pid>/cgroup` and returns the `.scope` directory name under `app.slice`,
/// strictly ignoring `.service`, `.socket`, `session.slice`, and `session-*.scope`.
pub fn parse_app_slice_scope(cgroup_text: &str, uid: u32) -> Option<String> {
    let prefix = format!("/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/");
    for line in cgroup_text.lines() {
        let path = line.splitn(3, ':').nth(2)?.trim();
        if let Some(rest) = path.strip_prefix(&prefix) {
            let unit = rest.split('/').next().unwrap_or("");
            if unit.ends_with(".scope") && !unit.is_empty() {
                return Some(unit.to_string());
            }
        }
    }
    None
}

fn scope_of_pid(pid: u32, uid: u32) -> Option<String> {
    let text = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    parse_app_slice_scope(&text, uid)
}

fn read_proc_comm(pid: u32) -> String {
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn is_wine_helper_comm(comm: &str) -> bool {
    matches!(
        comm,
        "explorer.exe"
            | "tabtip.exe"
            | "xalia.exe"
            | "conhost.exe"
            | "services.exe"
            | "winedevice.exe"
            | "plugplay.exe"
            | "svchost.exe"
            | "rpcss.exe"
            | "lsass.exe"
            | "agent.exe"
    )
}

fn is_vpn_or_tunnel_name(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    const TUNNELS: &[&str] = &[
        "vpn",
        "xray",
        "v2ray",
        "sing-box",
        "clash",
        "mihomo",
        "nekoray",
        "nekobox",
        "hiddify",
        "throne",
        "amnezia",
        "wireguard",
        "openvpn",
        "tailscale",
        "zerotier",
        "outline",
        "ssh",
    ];
    TUNNELS.iter().any(|k| lower.contains(k))
}

#[derive(Debug)]
struct X11Win {
    pid: u32,
    title: String,
    classes: Vec<String>,
    helper: bool,
}

fn query_x11_windows(display: &str) -> Vec<X11Win> {
    use x11rb::{
        connection::Connection,
        protocol::xproto::{AtomEnum, ConnectionExt as _, Window},
        rust_connection::RustConnection,
    };
    let Ok((conn, screen)) = RustConnection::connect(Some(display)) else {
        return Vec::new();
    };
    let Some(root) = conn.setup().roots.get(screen).map(|r| r.root) else {
        return Vec::new();
    };
    let intern = |name: &str| -> Option<u32> {
        Some(conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok()?.atom)
    };
    let Some(pid_atom) = intern("_NET_WM_PID") else {
        return Vec::new();
    };
    let net_wm_name = intern("_NET_WM_NAME").unwrap_or(0);
    let utf8_string = intern("UTF8_STRING").unwrap_or(0);

    let prop_bytes = |w: Window, atom: u32, ty: u32| -> Vec<u8> {
        conn.get_property(false, w, atom, ty, 0, 256)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.value)
            .unwrap_or_default()
    };

    let mut out = Vec::new();
    let mut stack = vec![(root, 0u8)];
    while let Some((win, depth)) = stack.pop() {
        let Ok(cookie) = conn.query_tree(win) else { continue };
        let Ok(tree) = cookie.reply() else { continue };
        for child in tree.children {
            let pid = conn
                .get_property(false, child, pid_atom, AtomEnum::CARDINAL, 0, 1)
                .ok()
                .and_then(|c| c.reply().ok())
                .and_then(|r| r.value32()?.next())
                .unwrap_or(0);
            if pid > 0 {
                let mut title_bytes = if net_wm_name != 0 && utf8_string != 0 {
                    prop_bytes(child, net_wm_name, utf8_string)
                } else {
                    Vec::new()
                };
                if title_bytes.is_empty() {
                    title_bytes = prop_bytes(child, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into());
                }
                let class_bytes = prop_bytes(child, AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into());
                let classes: Vec<String> = class_bytes
                    .split(|b| *b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect();
                let comm = read_proc_comm(pid);
                out.push(X11Win {
                    pid,
                    title: String::from_utf8_lossy(&title_bytes).into_owned(),
                    classes,
                    helper: is_wine_helper_comm(&comm),
                });
            }
            if depth < 2 {
                stack.push((child, depth + 1));
            }
        }
    }
    out
}

fn match_x11_pid(w: &WindowSample, x11_wins: &[X11Win]) -> Option<u32> {
    let class_matches = |x: &X11Win| {
        !w.app_id.is_empty() && x.classes.iter().any(|c| c.eq_ignore_ascii_case(&w.app_id))
    };
    // 1. Exact title + WM_CLASS on a real (non-helper) process
    if let Some(found) = x11_wins
        .iter()
        .find(|x| !x.helper && x.title == w.title && class_matches(x))
    {
        return Some(found.pid);
    }
    // 2. Exact title on a non-helper process
    if !w.title.is_empty() {
        if let Some(found) = x11_wins.iter().find(|x| !x.helper && x.title == w.title) {
            return Some(found.pid);
        }
    }
    // 3. WM_CLASS match on a non-helper process (e.g. game changed window title dynamically)
    if let Some(found) = x11_wins.iter().find(|x| !x.helper && class_matches(x)) {
        return Some(found.pid);
    }
    // 4. Fallback including helpers (still resolves the shared Wine/Proton `.scope`)
    x11_wins
        .iter()
        .find(|x| class_matches(x) || (!w.title.is_empty() && x.title == w.title))
        .map(|x| x.pid)
}

/// Fallback for Wine/Proton games whose X11 window did not set `_NET_WM_PID`:
/// finds non-helper `.exe` processes owned by `uid` inside `app.slice/*.scope`.
fn fallback_wine_game_scopes(uid: u32) -> Vec<(u32, String)> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let comm = read_proc_comm(pid);
        if !comm.ends_with(".exe") || is_wine_helper_comm(&comm) {
            continue;
        }
        if let Some(scope) = scope_of_pid(pid, uid) {
            found.push((pid, scope));
        }
    }
    found
}

/// Resolves the real host Linux PID of a PipeWire / PulseAudio client JSON object.
/// Inside `srt-bwrap` (Proton / Steam Linux Runtime) or Flatpak, `application.process.id`
/// reports the sandbox-internal PID (e.g. `950`), while `pipewire.sec.pid` holds the
/// kernel-verified `SO_PEERCRED` host PID (e.g. `58959` for `Wow.exe`). Conversely, for
/// `pipewire-pulse` bridge clients `pipewire.sec.pid` is `pipewire-pulse` itself and
/// `application.process.id` is the real client PID.
pub fn resolve_pactl_client_pid(props: &serde_json::Value) -> Option<u32> {
    let bin = props["application.process.binary"]
        .as_str()
        .unwrap_or("")
        .trim();
    if matches!(bin, "pipewire" | "pipewire-pulse" | "wireplumber" | "pactl") {
        return None;
    }
    let app_pid = props["application.process.id"]
        .as_str()
        .and_then(|s| s.trim().parse::<u32>().ok());
    let sec_pid = props["pipewire.sec.pid"]
        .as_str()
        .and_then(|s| s.trim().parse::<u32>().ok());
    if props["client.api"].as_str() == Some("pipewire-pulse") {
        app_pid.or(sec_pid)
    } else {
        sec_pid.or(app_pid)
    }
}

#[derive(Debug, Clone)]
struct SinkInputInfo {
    index: u32,
    muted: bool,
    corked: bool,
    host_pid: Option<u32>,
    scope: Option<String>,
    app_name: String,
    restore_key: String,
}

#[derive(Default)]
struct AudioSnapshot {
    /// Uncorked, unmuted playback or capture PIDs (voice chat, music, etc.)
    active_pids: HashSet<u32>,
    sink_inputs: Vec<SinkInputInfo>,
}

fn run_pactl_json(kind: &str, deadline: Instant) -> Option<serde_json::Value> {
    let mut child = Command::new("pactl")
        .env("LC_ALL", "C")
        .args(["--format=json", "list", kind])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(512 * 1024).read_to_end(&mut buf);
        buf
    });
    let ok = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(8));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    if !ok {
        return None;
    }
    let bytes = reader.join().ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn set_sink_input_mute(index: u32, mute: bool) {
    let _ = Command::new("pactl")
        .env("LC_ALL", "C")
        .args(["set-sink-input-mute", &index.to_string(), if mute { "1" } else { "0" }])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Queries playback (`sink-inputs`), capture (`source-outputs`) and `clients` via `pactl`
/// both to exempt active voice/music streams and to mute/unmute backgrounded games cleanly.
fn query_audio_snapshot(uid: u32) -> AudioSnapshot {
    let deadline = Instant::now() + Duration::from_millis(400);
    let mut client_pids: HashMap<u32, u32> = HashMap::new();
    if let Some(clients) = run_pactl_json("clients", deadline).and_then(|v| v.as_array().cloned()) {
        for c in clients {
            let Some(idx) = c["index"].as_u64().map(|v| v as u32) else {
                continue;
            };
            if let Some(pid) = resolve_pactl_client_pid(&c["properties"]) {
                client_pids.insert(idx, pid);
            }
        }
    }

    let parse_client_id = |item: &serde_json::Value| -> Option<u32> {
        item["client"]
            .as_str()
            .and_then(|s| s.parse::<u32>().ok())
            .or_else(|| item["client"].as_u64().map(|n| n as u32))
    };

    let mut out = AudioSnapshot::default();

    if let Some(arr) = run_pactl_json("sink-inputs", deadline).and_then(|v| v.as_array().cloned()) {
        for item in arr {
            let props = &item["properties"];
            if props["stream.monitor"].as_str() == Some("true")
                || props["media.role"].as_str() == Some("event")
            {
                continue;
            }
            let Some(index) = item["index"].as_u64().map(|v| v as u32) else {
                continue;
            };
            let corked = item["corked"].as_bool().unwrap_or(false);
            let muted = item["mute"].as_bool().unwrap_or(false);
            let host_pid = parse_client_id(&item)
                .and_then(|cid| client_pids.get(&cid).copied())
                .or_else(|| resolve_pactl_client_pid(props));
            if !corked && !muted {
                if let Some(pid) = host_pid {
                    out.active_pids.insert(pid);
                }
            }
            let app_name = ["application.name", "node.name", "media.name"]
                .into_iter()
                .find_map(|k| props[k].as_str().map(str::trim).filter(|s| !s.is_empty()))
                .unwrap_or("")
                .to_string();
            let restore_key = props["module-stream-restore.id"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| app_name.to_ascii_lowercase());
            let scope = host_pid.and_then(|p| scope_of_pid(p, uid));
            out.sink_inputs.push(SinkInputInfo {
                index,
                muted,
                corked,
                host_pid,
                scope,
                app_name,
                restore_key,
            });
        }
    }

    if let Some(arr) = run_pactl_json("source-outputs", deadline).and_then(|v| v.as_array().cloned())
    {
        for item in arr {
            if item["corked"].as_bool() == Some(true) || item["mute"].as_bool() == Some(true) {
                continue;
            }
            let props = &item["properties"];
            if props["stream.monitor"].as_str() == Some("true")
                || props["media.name"].as_str() == Some("Peak detect")
            {
                continue;
            }
            if let Some(pid) = parse_client_id(&item)
                .and_then(|cid| client_pids.get(&cid).copied())
                .or_else(|| resolve_pactl_client_pid(props))
            {
                out.active_pids.insert(pid);
            }
        }
    }

    out
}

fn list_active_app_scopes(app_slice: &Path) -> Vec<(String, Vec<u32>)> {
    let Ok(entries) = fs::read_dir(app_slice) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.ends_with(".scope") {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let procs_text = fs::read_to_string(path.join("cgroup.procs")).unwrap_or_default();
        let pids: Vec<u32> = procs_text
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .collect();
        if !pids.is_empty() {
            out.push((name, pids));
        }
    }
    out
}

fn set_pid_idle_priority(pid: i32, idle: bool) {
    if pid <= 1 {
        return;
    }
    unsafe {
        let param = libc::sched_param { sched_priority: 0 };
        if idle {
            libc::sched_setscheduler(pid, libc::SCHED_IDLE, &param);
            libc::setpriority(libc::PRIO_PROCESS, pid as u32, 15);
        } else {
            libc::sched_setscheduler(pid, libc::SCHED_OTHER, &param);
            libc::setpriority(libc::PRIO_PROCESS, pid as u32, 0);
        }
    }
}

fn session_members_of(leader_or_member_pid: i32, exclude: &HashSet<i32>) -> Vec<i32> {
    let Some(sid) = feather_ui::process::session_of(leader_or_member_pid) else {
        return Vec::new();
    };
    // Never throttle the compositor's own session!
    let comp_sid = unsafe { libc::getsid(0) };
    if sid == comp_sid || sid <= 1 {
        return Vec::new();
    }
    let Ok(dir) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|pid| !exclude.contains(pid) && feather_ui::process::session_of(*pid) == Some(sid))
        .collect()
}

fn apply_snapshot(state: &mut WorkerState, snapshot: &Snapshot) {
    let any_game = snapshot.enabled && snapshot.windows.iter().any(|w| w.is_game);
    let any_hidden = snapshot.enabled && snapshot.windows.iter().any(|w| !w.shown && !w.focused);
    if !snapshot.enabled || (!any_game && !any_hidden) {
        state.restore_all();
        return;
    }

    let uid = unsafe { libc::getuid() };
    let need_x11 = snapshot.windows.iter().any(|w| w.xwayland);
    let x11_wins = match (&snapshot.x11_display, need_x11) {
        (Some(d), true) => query_x11_windows(d),
        _ => Vec::new(),
    };

    let mut focused_game_scopes = HashSet::new();
    let mut unfocused_game_scopes = HashSet::new();
    let mut focused_desktop_scopes = HashSet::new();
    let mut visible_desktop_scopes = HashSet::new();
    let mut windowed_scopes = HashSet::new();
    let mut game_pids: Vec<(u32, bool)> = Vec::new();
    let mut bg_window_pids: Vec<u32> = Vec::new();
    let mut fg_window_pids: Vec<u32> = Vec::new();
    let mut visible_window_pids: Vec<u32> = Vec::new();

    for w in &snapshot.windows {
        let resolved_pid = if w.xwayland {
            match_x11_pid(w, &x11_wins)
        } else {
            w.wl_client_pid
        };
        let scope = resolved_pid.and_then(|p| scope_of_pid(p, uid));
        if let Some(ref s) = scope {
            windowed_scopes.insert(s.clone());
        }
        if w.is_game {
            if let Some(p) = resolved_pid {
                game_pids.push((p, w.focused));
            }
            if let Some(s) = scope {
                if w.focused {
                    focused_game_scopes.insert(s);
                } else {
                    unfocused_game_scopes.insert(s);
                }
            } else {
                // Fallback for Wine/Proton games without `_NET_WM_PID`
                for (p, s) in fallback_wine_game_scopes(uid) {
                    game_pids.push((p, w.focused));
                    windowed_scopes.insert(s.clone());
                    if w.focused {
                        focused_game_scopes.insert(s);
                    } else {
                        unfocused_game_scopes.insert(s);
                    }
                }
            }
        } else if w.focused {
            if let Some(p) = resolved_pid {
                fg_window_pids.push(p);
            }
            if let Some(s) = scope {
                focused_desktop_scopes.insert(s);
            }
        } else if w.shown {
            if let Some(p) = resolved_pid {
                visible_window_pids.push(p);
            }
            if let Some(s) = scope {
                visible_desktop_scopes.insert(s);
            }
        } else if let Some(p) = resolved_pid {
            bg_window_pids.push(p);
        }
    }

    let has_focused_game = !focused_game_scopes.is_empty()
        || game_pids.iter().any(|&(_, focused)| focused);

    let audio_snap = query_audio_snapshot(uid);
    // Always exempt active playback/capture streams (music, voice chat, video) from both
    // Game Mode throttling and desktop App Nap.
    let audio_pids = audio_snap.active_pids.clone();

    // 0. Automatically mute unfocused/minimized games and unmute focused games (or restore if
    //    no game remains open).
    if any_game {
        let focused_game_pid_set: HashSet<u32> = game_pids
            .iter()
            .filter(|&&(_, focused)| focused)
            .map(|&(p, _)| p)
            .collect();
        let unfocused_game_pid_set: HashSet<u32> = game_pids
            .iter()
            .filter(|&&(_, focused)| !focused)
            .map(|&(p, _)| p)
            .collect();
        let focused_game_titles: Vec<&str> = snapshot
            .windows
            .iter()
            .filter(|w| w.is_game && w.focused && !w.title.is_empty())
            .map(|w| w.title.as_str())
            .collect();
        let unfocused_game_titles: Vec<&str> = snapshot
            .windows
            .iter()
            .filter(|w| w.is_game && !w.focused && !w.title.is_empty())
            .map(|w| w.title.as_str())
            .collect();

        for si in &audio_snap.sink_inputs {
            let is_fg_game = si
                .scope
                .as_ref()
                .is_some_and(|s| focused_game_scopes.contains(s))
                || si.host_pid.is_some_and(|p| focused_game_pid_set.contains(&p))
                || (!si.app_name.is_empty()
                    && focused_game_titles
                        .iter()
                        .any(|t| si.app_name.eq_ignore_ascii_case(t)));
            let is_bg_game = !is_fg_game
                && (si
                    .scope
                    .as_ref()
                    .is_some_and(|s| unfocused_game_scopes.contains(s))
                    || si
                        .host_pid
                        .is_some_and(|p| unfocused_game_pid_set.contains(&p))
                    || (!si.app_name.is_empty()
                        && unfocused_game_titles
                            .iter()
                            .any(|t| si.app_name.eq_ignore_ascii_case(t))));

            if is_fg_game {
                if si.muted
                    && (state.auto_muted_indices.contains(&si.index)
                        || (!si.restore_key.is_empty()
                            && state.auto_muted_keys.contains(&si.restore_key)))
                {
                    set_sink_input_mute(si.index, false);
                    state.auto_muted_indices.remove(&si.index);
                    state.auto_muted_keys.remove(&si.restore_key);
                    state.persist_muted_keys();
                }
            } else if is_bg_game && !si.corked {
                if !si.muted {
                    set_sink_input_mute(si.index, true);
                    state.auto_muted_indices.insert(si.index);
                    if !si.restore_key.is_empty() {
                        state.auto_muted_keys.insert(si.restore_key.clone());
                        state.persist_muted_keys();
                    }
                }
            }
        }
    } else if !state.auto_muted_indices.is_empty() || !state.auto_muted_keys.is_empty() {
        for si in &audio_snap.sink_inputs {
            if si.muted
                && (state.auto_muted_indices.contains(&si.index)
                    || (!si.restore_key.is_empty()
                        && state.auto_muted_keys.contains(&si.restore_key)))
            {
                set_sink_input_mute(si.index, false);
                state.auto_muted_keys.remove(&si.restore_key);
            }
        }
        state.auto_muted_indices.clear();
        state.persist_muted_keys();
    }

    // 1. Apply cgroup v2 `.scope` policies inside `app.slice`
    if let Some(app_slice) = user_app_slice_dir() {
        if ensure_subtree_cpu_controller(&app_slice, &mut state.subtree_cpu_enabled) {
            let active_scopes = list_active_app_scopes(&app_slice);
            let mut exempt_scopes = HashSet::new();
            for (name, pids) in &active_scopes {
                if is_vpn_or_tunnel_name(name)
                    || pids.iter().any(|p| {
                        audio_pids.contains(p) || is_vpn_or_tunnel_name(&read_proc_comm(*p))
                    })
                {
                    exempt_scopes.insert(name.clone());
                }
            }
            // Chromium / Electron move their main process into `app-*.scope` while leaving their
            // Zygote, GPU and YouTube video-decoder renderers in the initial `run-p*.scope`
            // sharing the same Linux session ID (`sid`). Propagate foreground/visible/windowed
            // membership and audio exemption across sibling scopes of the same session.
            let comp_sid = unsafe { libc::getsid(0) };
            let scope_sids: Vec<(String, HashSet<i32>)> = active_scopes
                .iter()
                .map(|(name, pids)| {
                    let sids = pids
                        .iter()
                        .take(8)
                        .filter_map(|&p| feather_ui::process::session_of(p as i32))
                        .filter(|&sid| sid > 1 && sid != comp_sid)
                        .collect();
                    (name.clone(), sids)
                })
                .collect();
            let mut fg_sids = HashSet::new();
            let mut vis_sids = HashSet::new();
            let mut win_sids = HashSet::new();
            let mut exempt_sids = HashSet::new();
            for (name, sids) in &scope_sids {
                if focused_desktop_scopes.contains(name) {
                    fg_sids.extend(sids.iter().copied());
                }
                if visible_desktop_scopes.contains(name) {
                    vis_sids.extend(sids.iter().copied());
                }
                if windowed_scopes.contains(name) {
                    win_sids.extend(sids.iter().copied());
                }
                if exempt_scopes.contains(name) {
                    exempt_sids.extend(sids.iter().copied());
                }
            }
            for (name, sids) in &scope_sids {
                if sids.iter().any(|sid| fg_sids.contains(sid)) {
                    focused_desktop_scopes.insert(name.clone());
                }
                if sids.iter().any(|sid| vis_sids.contains(sid)) {
                    visible_desktop_scopes.insert(name.clone());
                }
                if sids.iter().any(|sid| win_sids.contains(sid)) {
                    windowed_scopes.insert(name.clone());
                }
                if sids.iter().any(|sid| exempt_sids.contains(sid)) {
                    exempt_scopes.insert(name.clone());
                }
            }
            let scope_names: Vec<String> = active_scopes.iter().map(|(n, _)| n.clone()).collect();
            let planned = plan_scope_roles(
                &scope_names,
                &focused_game_scopes,
                &unfocused_game_scopes,
                &focused_desktop_scopes,
                &visible_desktop_scopes,
                &windowed_scopes,
                &exempt_scopes,
            );

            // Restore scopes that disappeared from the active set or returned to Normal
            let prev_paths: Vec<PathBuf> = state.touched_scopes.keys().cloned().collect();
            for path in prev_paths {
                let keep = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| planned.get(n))
                    .copied();
                if keep.is_none() && path.is_dir() {
                    write_scope_role(&path, ScopeRole::Normal);
                    state.touched_scopes.remove(&path);
                }
            }

            for (name, role) in planned {
                let path = app_slice.join(&name);
                if state.touched_scopes.get(&path).copied() != Some(role) {
                    if role == ScopeRole::Normal && !state.touched_scopes.contains_key(&path) {
                        continue;
                    }
                    write_scope_role(&path, role);
                    if role == ScopeRole::Normal {
                        state.touched_scopes.remove(&path);
                    } else {
                        state.touched_scopes.insert(path, role);
                    }
                }
            }
        }
    }

    // 2. Fallback per-PID scheduler priority (`SCHED_IDLE` / `nice 15`) for processes outside
    // `app.slice` (e.g. apps started in `session-*.scope` before scope delegation).
    let mut protected = HashSet::new();
    protected.insert(std::process::id() as i32);
    if let Some(sat) = snapshot.satellite_pid {
        protected.insert(sat as i32);
    }
    for &p in &audio_pids {
        protected.insert(p as i32);
    }
    for &p in &fg_window_pids {
        protected.insert(p as i32);
    }
    if !has_focused_game {
        for &p in &visible_window_pids {
            protected.insert(p as i32);
        }
    }
    for &(p, focused) in &game_pids {
        if focused {
            protected.insert(p as i32);
        }
    }

    let mut desired_idle_pids = HashSet::new();
    for &(p, focused) in &game_pids {
        if !focused && scope_of_pid(p, uid).is_none() && !protected.contains(&(p as i32)) {
            desired_idle_pids.insert(p as i32);
            for m in session_members_of(p as i32, &protected) {
                if !is_vpn_or_tunnel_name(&read_proc_comm(m as u32)) {
                    desired_idle_pids.insert(m);
                }
            }
        }
    }
    if has_focused_game || any_hidden {
        for p in bg_window_pids {
            if scope_of_pid(p, uid).is_none()
                && !protected.contains(&(p as i32))
                && !is_vpn_or_tunnel_name(&read_proc_comm(p))
            {
                for m in session_members_of(p as i32, &protected) {
                    if !is_vpn_or_tunnel_name(&read_proc_comm(m as u32)) {
                        desired_idle_pids.insert(m);
                    }
                }
                desired_idle_pids.insert(p as i32);
            }
        }
    }

    let stale: Vec<i32> = state
        .touched_pids
        .iter()
        .copied()
        .filter(|p| !desired_idle_pids.contains(p))
        .collect();
    for p in stale {
        set_pid_idle_priority(p, false);
        state.touched_pids.remove(&p);
    }
    for p in desired_idle_pids {
        if state.touched_pids.insert(p) {
            set_pid_idle_priority(p, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_cpu_max_quotas_respect_linux_cfs_minimum() {
        // Linux kernel (`kernel/sched/core.c`) rejects CFS bandwidth quotas < 1000 µs with EINVAL.
        let roles = [
            ScopeRole::Normal,
            ScopeRole::ForegroundDesktop,
            ScopeRole::FocusedGame,
            ScopeRole::BackgroundHiddenApp,
            ScopeRole::BackgroundGame,
            ScopeRole::BackgroundAppDuringGame,
        ];
        for role in roles {
            let (cpu_max, _, _, _) = role.params();
            let mut parts = cpu_max.split_whitespace();
            let quota = parts.next().expect("quota token");
            let period: u64 = parts
                .next()
                .expect("period token")
                .parse()
                .expect("numeric period");
            assert!(
                (1000..=1_000_000).contains(&period),
                "{role:?} period {period} out of Linux CFS range"
            );
            if quota != "max" {
                let q: u64 = quota.parse().expect("numeric quota");
                assert!(
                    q >= 1000,
                    "{role:?} quota {q} < 1000 µs would be rejected with EINVAL by Linux kernel"
                );
                assert!(q <= period, "{role:?} quota {q} exceeds period {period}");
            }
        }
    }

    #[test]
    fn detects_proton_wine_and_skips_launchers_and_browsers() {
        // World of Warcraft via Heroic / Proton
        assert!(classify_game_window(
            "steam_app_default",
            "World of Warcraft",
            true,
            true,
            false,
            false,
            false,
            false,
            false,
            false,
        ));
        // Windowed/borderless Steam Proton game is still a game
        assert!(classify_game_window(
            "steam_app_730",
            "Counter-Strike 2",
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
        ));
        // Standalone Wine .exe
        assert!(classify_game_window(
            "wow.exe",
            "World of Warcraft",
            false,
            true,
            false,
            false,
            false,
            false,
            false,
            false,
        ));
        // Battle.net launcher sharing steam_app_default must NOT be treated as a 3D game
        assert!(!classify_game_window(
            "steam_app_default",
            "Battle.net",
            false,
            true,
            false,
            false,
            false,
            false,
            false,
            false,
        ));
        // Heroic / Steam launcher main window (even with Categories=Game;) is NOT treated as a game
        assert!(!classify_game_window(
            "heroic",
            "Heroic Games Launcher",
            false,
            false,
            true,
            false,
            false,
            false,
            false,
            false,
        ));
        assert!(!classify_game_window(
            "steam",
            "Steam",
            false,
            true,
            true,
            false,
            false,
            false,
            false,
            false,
        ));
        // Fullscreen YouTube in Google Chrome or mpv is NOT throttled as a 3D game
        assert!(!classify_game_window(
            "google-chrome",
            "YouTube - Google Chrome",
            true,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
        ));
        // Proton auxiliary helper window is ignored
        assert!(!classify_game_window(
            "steam_app_default",
            "Окно Wayland",
            false,
            true,
            false,
            false,
            false,
            false,
            false,
            true,
        ));
    }

    #[test]
    fn parses_strict_app_slice_scopes_only() {
        let cg = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-heroic-13561.scope\n";
        assert_eq!(
            parse_app_slice_scope(cg, 1000).as_deref(),
            Some("app-heroic-13561.scope")
        );
        // Services and session.slice must never be touched
        let svc = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/dconf.service\n";
        assert_eq!(parse_app_slice_scope(svc, 1000), None);
        let pipewire =
            "0::/user.slice/user-1000.slice/user@1000.service/session.slice/pipewire.service\n";
        assert_eq!(parse_app_slice_scope(pipewire, 1000), None);
        let session = "0::/user.slice/user-1000.slice/session-1.scope\n";
        assert_eq!(parse_app_slice_scope(session, 1000), None);
    }

    #[test]
    fn plans_roles_for_focused_alt_tabbed_game_and_app_nap() {
        let scopes = vec![
            "app-heroic-13561.scope".to_string(),
            "app-com.google.Chrome-6343.scope".to_string(),
            "snap.telegram-desktop.1.scope".to_string(),
            "app-antigravity-19468.scope".to_string(),
            "app-discord-voice.scope".to_string(),
            "app-mousevpn.scope".to_string(),
            "run-daemon-helper.scope".to_string(),
        ];
        let exempt: HashSet<String> = [
            "app-discord-voice.scope".to_string(),
            "app-mousevpn.scope".to_string(),
        ]
        .into_iter()
        .collect();
        let windowed: HashSet<String> = [
            "app-heroic-13561.scope".to_string(),
            "app-com.google.Chrome-6343.scope".to_string(),
            "snap.telegram-desktop.1.scope".to_string(),
            "app-antigravity-19468.scope".to_string(),
            "app-discord-voice.scope".to_string(),
            "app-mousevpn.scope".to_string(),
        ]
        .into_iter()
        .collect();

        // 1. WoW is actively focused: WoW gets FocusedGame, Discord/VPN stay Normal,
        // Chrome, Telegram & Antigravity get BackgroundAppDuringGame.
        let fg_game: HashSet<String> = ["app-heroic-13561.scope".to_string()].into_iter().collect();
        let roles = plan_scope_roles(
            &scopes,
            &fg_game,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &windowed,
            &exempt,
        );
        assert_eq!(roles["app-heroic-13561.scope"], ScopeRole::FocusedGame);
        assert_eq!(
            roles["app-com.google.Chrome-6343.scope"],
            ScopeRole::BackgroundAppDuringGame
        );
        assert_eq!(
            roles["snap.telegram-desktop.1.scope"],
            ScopeRole::BackgroundAppDuringGame
        );
        assert_eq!(roles["app-discord-voice.scope"], ScopeRole::Normal);
        assert_eq!(roles["app-mousevpn.scope"], ScopeRole::Normal);

        // 2. User Alt+Tabs to maximized Chrome: WoW drops to BackgroundGame (valid CFS quota),
        // Chrome gets ForegroundDesktop, occluded Telegram gets App Nap (BackgroundHiddenApp),
        // visible side-by-side Antigravity stays Normal, Discord/VPN & non-windowed helpers stay Normal!
        let bg_game: HashSet<String> = ["app-heroic-13561.scope".to_string()].into_iter().collect();
        let fg_desk: HashSet<String> = ["app-com.google.Chrome-6343.scope".to_string()]
            .into_iter()
            .collect();
        let vis_desk: HashSet<String> = ["app-antigravity-19468.scope".to_string()]
            .into_iter()
            .collect();
        let roles2 = plan_scope_roles(
            &scopes,
            &HashSet::new(),
            &bg_game,
            &fg_desk,
            &vis_desk,
            &windowed,
            &exempt,
        );
        assert_eq!(roles2["app-heroic-13561.scope"], ScopeRole::BackgroundGame);
        assert_eq!(
            roles2["app-com.google.Chrome-6343.scope"],
            ScopeRole::ForegroundDesktop
        );
        assert_eq!(
            roles2["snap.telegram-desktop.1.scope"],
            ScopeRole::BackgroundHiddenApp
        );
        assert_eq!(roles2["app-antigravity-19468.scope"], ScopeRole::Normal);
        assert_eq!(roles2["app-discord-voice.scope"], ScopeRole::Normal);
        assert_eq!(roles2["app-mousevpn.scope"], ScopeRole::Normal);
        assert_eq!(roles2["run-daemon-helper.scope"], ScopeRole::Normal);

        // 3. Even with NO game running at all, occluded Telegram gets App Nap while focused
        // Chrome gets ForegroundDesktop and visible/audio/VPN/helper scopes remain Normal.
        let roles3 = plan_scope_roles(
            &scopes,
            &HashSet::new(),
            &HashSet::new(),
            &fg_desk,
            &vis_desk,
            &windowed,
            &exempt,
        );
        assert_eq!(
            roles3["app-com.google.Chrome-6343.scope"],
            ScopeRole::ForegroundDesktop
        );
        assert_eq!(
            roles3["snap.telegram-desktop.1.scope"],
            ScopeRole::BackgroundHiddenApp
        );
        assert_eq!(roles3["app-antigravity-19468.scope"], ScopeRole::Normal);
        assert_eq!(roles3["app-discord-voice.scope"], ScopeRole::Normal);
        assert_eq!(roles3["run-daemon-helper.scope"], ScopeRole::Normal);
    }

    #[test]
    fn writes_and_restores_mock_cgroup_files() {
        let tmp = std::env::temp_dir().join(format!("feather-cg-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("memory.low"), "0").unwrap();

        write_scope_role(&tmp, ScopeRole::ForegroundDesktop);
        let fg_mem: u64 = fs::read_to_string(tmp.join("memory.low"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(fg_mem >= 512 * 1024 * 1024);
        assert_eq!(fs::read_to_string(tmp.join("cpu.weight")).unwrap(), "500");

        write_scope_role(&tmp, ScopeRole::BackgroundGame);
        assert_eq!(
            fs::read_to_string(tmp.join("cpu.max")).unwrap(),
            UNFOCUSED_GAME_CPU_MAX
        );
        assert_eq!(fs::read_to_string(tmp.join("cpu.idle")).unwrap(), "1");
        assert_eq!(fs::read_to_string(tmp.join("memory.low")).unwrap(), "0");

        write_scope_role(&tmp, ScopeRole::BackgroundHiddenApp);
        assert_eq!(
            fs::read_to_string(tmp.join("cpu.max")).unwrap(),
            UNLIMITED_CPU_MAX
        );
        assert_eq!(fs::read_to_string(tmp.join("cpu.idle")).unwrap(), "1");

        write_scope_role(&tmp, ScopeRole::Normal);
        assert_eq!(
            fs::read_to_string(tmp.join("cpu.max")).unwrap(),
            UNLIMITED_CPU_MAX
        );
        assert_eq!(fs::read_to_string(tmp.join("cpu.idle")).unwrap(), "0");
        assert_eq!(fs::read_to_string(tmp.join("cpu.weight")).unwrap(), "100");
        assert_eq!(fs::read_to_string(tmp.join("memory.low")).unwrap(), "0");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolves_bwrap_proton_and_pipewire_pulse_host_pids() {
        // Proton/Heroic inside srt-bwrap: application.process.id is namespace PID 950,
        // while pipewire.sec.pid is the real host PID 58959!
        let proton_client: serde_json::Value = serde_json::json!({
            "pipewire.sec.pid": "58959",
            "application.name": "World of Warcraft",
            "application.process.binary": "wine64-preloader",
            "application.process.id": "950"
        });
        assert_eq!(resolve_pactl_client_pid(&proton_client), Some(58959));

        // pipewire-pulse client (Chrome/Discord): pipewire.sec.pid is pipewire-pulse (5215),
        // while application.process.id is the real host PID (60394).
        let pulse_client: serde_json::Value = serde_json::json!({
            "client.api": "pipewire-pulse",
            "pipewire.sec.pid": "5215",
            "application.process.binary": "chrome",
            "application.process.id": "60394"
        });
        assert_eq!(resolve_pactl_client_pid(&pulse_client), Some(60394));

        // PipeWire/WirePlumber daemons themselves are ignored
        let daemon: serde_json::Value = serde_json::json!({
            "pipewire.sec.pid": "5213",
            "application.process.binary": "wireplumber",
            "application.process.id": "5213"
        });
        assert_eq!(resolve_pactl_client_pid(&daemon), None);
    }
}
