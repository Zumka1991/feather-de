//! X11 programs through xwayland-satellite, started on demand.
//!
//! The compositor owns the X display: it takes a free `:N`, writes the lock file and
//! listens on both X11 sockets (the path one and the abstract one). Nothing runs until an
//! X client connects; then xwayland-satellite starts with those sockets (`-listenfd`) and
//! connects back to us as an ordinary Wayland client, so X windows are xdg toplevels.
//! When it exits the sockets are watched again. No X program: no process, no memory.
use crate::FeatherCompositor;
use smithay::reexports::calloop::{
    channel,
    generic::Generic,
    Interest, LoopHandle, Mode, PostAction, RegistrationToken,
};
use std::{
    os::{
        fd::AsRawFd,
        linux::net::SocketAddrExt,
        unix::{net::{SocketAddr, UnixListener}, process::CommandExt},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const SOCKET_DIR: &str = "/tmp/.X11-unix";
/// Quick deaths in a row before giving up (a broken install must not respawn forever)
const MAX_FAILURES: u32 = 3;

static X_DISPLAY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Socket and lock paths for the signal handler, NUL-terminated, set before it is installed
static mut CLEANUP: [[u8; 32]; 2] = [[0; 32]; 2];

extern "C" fn on_terminate(sig: libc::c_int) {
    // Only async-signal-safe calls: unlink, then die of the same signal as before
    unsafe {
        let paths = &*std::ptr::addr_of!(CLEANUP);
        for path in paths {
            if path[0] != 0 { libc::unlink(path.as_ptr().cast()); }
        }
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

/// SIGTERM/SIGINT/SIGHUP (logout, `timeout`, Ctrl+C) remove our X socket and lock too
fn install_cleanup(n: u32) {
    let (socket, lock) = (format!("{SOCKET_DIR}/X{n}"), format!("/tmp/.X{n}-lock"));
    unsafe {
        let paths = &mut *std::ptr::addr_of_mut!(CLEANUP);
        for (slot, path) in paths.iter_mut().zip([socket, lock]) {
            let bytes = path.as_bytes();
            if bytes.len() < slot.len() {
                slot[..bytes.len()].copy_from_slice(bytes);
                slot[bytes.len()] = 0;
            }
        }
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            // Keep a handler someone else installed; only replace the default action
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, std::ptr::null(), &mut old) == 0 && old.sa_sigaction == libc::SIG_DFL {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = on_terminate as *const () as usize;
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaction(sig, &action, std::ptr::null_mut());
            }
        }
    }
}

/// `DISPLAY` for programs we launch, when our X display is up
pub fn display() -> Option<String> {
    X_DISPLAY.lock().unwrap().clone()
}

pub struct Xwayland {
    number: u32,
    satellite: PathBuf,
    wayland_socket: String,
    /// Kept open for the whole session: satellite accepts on them, we only watch
    listeners: [UnixListener; 2],
    tokens: Vec<RegistrationToken>,
    running: bool,
    started: Instant,
    failures: u32,
}

/// The first `:N` whose lock is free; a lock of a dead process is taken over like Xorg does
fn claim_display() -> Option<(u32, UnixListener, UnixListener)> {
    let _ = std::fs::create_dir(SOCKET_DIR);
    for n in 0..64u32 {
        let lock = lock_path(n);
        if lock.exists() {
            if lock_owner_alive(&lock) {
                continue;
            }
            // Stale: its owner is gone
            let _ = std::fs::remove_file(&lock);
            let _ = std::fs::remove_file(socket_path(n));
        }
        let Ok(mut file) = std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) else { continue };
        let _ = std::io::Write::write_all(&mut file, format!("{:>10}\n", std::process::id()).as_bytes());
        let path = socket_path(n);
        let _ = std::fs::remove_file(&path);
        let bound = UnixListener::bind(&path).and_then(|unix| {
            let name = format!("/tmp/.X11-unix/X{n}");
            let abstract_ = UnixListener::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes())?)?;
            Ok((unix, abstract_))
        });
        match bound {
            Ok((unix, abstract_)) => return Some((n, unix, abstract_)),
            Err(_) => {
                // Someone else's abstract socket (an X server in another namespace): next
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(&lock);
            }
        }
    }
    None
}

/// An unreadable or foreign-format lock counts as alive: never steal what we do not understand
fn lock_owner_alive(lock: &Path) -> bool {
    match std::fs::read_to_string(lock).ok().map(|s| s.trim().parse::<u32>()) {
        Some(Ok(pid)) => Path::new(&format!("/proc/{pid}")).exists(),
        _ => true,
    }
}

fn lock_path(n: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/.X{n}-lock"))
}
fn socket_path(n: u32) -> PathBuf {
    PathBuf::from(format!("{SOCKET_DIR}/X{n}"))
}

fn find_satellite() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FEATHER_XWAYLAND_SATELLITE").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join("xwayland-satellite"))
        .find(|p| p.is_file())
}

impl Xwayland {
    /// Claims a display and starts watching it; None without xwayland-satellite
    pub fn new(handle: &LoopHandle<'static, FeatherCompositor>, wayland_socket: &str, update_activation_env: bool) -> Option<Self> {
        let satellite = find_satellite()?;
        let (number, unix, abstract_) = claim_display()?;
        let display = format!(":{number}");
        *X_DISPLAY.lock().unwrap() = Some(display.clone());
        install_cleanup(number);
        if update_activation_env {
            // D-Bus/systemd-activated programs need DISPLAY too; off the event loop
            let _ = std::thread::Builder::new().name("feather-x-env".into()).spawn(move || {
                let _ = std::process::Command::new("dbus-update-activation-environment")
                    .args(["--systemd", &format!("DISPLAY={display}")])
                    .status();
            });
        }
        tracing::info!("XWayland: DISPLAY=:{number}, xwayland-satellite запустится по первому X-клиенту");
        let mut x = Self {
            number,
            satellite,
            wayland_socket: wayland_socket.to_owned(),
            listeners: [unix, abstract_],
            tokens: Vec::new(),
            running: false,
            started: Instant::now(),
            failures: 0,
        };
        x.watch(handle);
        Some(x)
    }

    fn watch(&mut self, handle: &LoopHandle<'static, FeatherCompositor>) {
        for listener in &self.listeners {
            let Ok(fd) = listener.try_clone() else { continue };
            let source = Generic::new(fd, Interest::READ, Mode::Level);
            if let Ok(token) = handle.insert_source(source, |_, _, state| {
                state.xwayland_wanted();
                Ok(PostAction::Continue)
            }) {
                self.tokens.push(token);
            }
        }
    }

    fn unwatch(&mut self, handle: &LoopHandle<'static, FeatherCompositor>) {
        for token in self.tokens.drain(..) {
            handle.remove(token);
        }
    }

    /// An X client is waiting: hand the sockets to xwayland-satellite
    fn spawn(&mut self, handle: &LoopHandle<'static, FeatherCompositor>) {
        self.unwatch(handle);
        let fds = [self.listeners[0].as_raw_fd(), self.listeners[1].as_raw_fd()];
        let mut cmd = std::process::Command::new(&self.satellite);
        cmd.arg(format!(":{}", self.number));
        for fd in fds {
            cmd.arg("-listenfd").arg(fd.to_string());
        }
        cmd.env("WAYLAND_DISPLAY", &self.wayland_socket)
            .env_remove("DISPLAY")
            .stdin(std::process::Stdio::null());
        // SAFETY: only async-signal-safe fcntl calls between fork and exec
        unsafe {
            cmd.pre_exec(move || {
                for fd in fds {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        match cmd.spawn() {
            Ok(mut child) => {
                self.running = true;
                self.started = Instant::now();
                tracing::info!("XWayland: xwayland-satellite запущен (pid {})", child.id());
                let (tx, rx) = channel::channel::<()>();
                let _ = handle.insert_source(rx, |event, _, state| {
                    if let channel::Event::Msg(()) = event {
                        state.xwayland_exited();
                    }
                });
                let _ = std::thread::Builder::new()
                    .name("feather-xwayland".into())
                    .stack_size(32 * 1024)
                    .spawn(move || {
                        let _ = child.wait();
                        let _ = tx.send(());
                    });
            }
            Err(e) => {
                tracing::warn!("XWayland: не удалось запустить xwayland-satellite: {e}");
                self.failures = MAX_FAILURES;
                self.shutdown();
            }
        }
    }

    fn exited(&mut self, handle: &LoopHandle<'static, FeatherCompositor>) -> bool {
        self.running = false;
        let quick = self.started.elapsed() < Duration::from_secs(3);
        self.failures = if quick { self.failures + 1 } else { 0 };
        if self.failures >= MAX_FAILURES {
            tracing::warn!("XWayland: xwayland-satellite завершается сразу после запуска, X-программы отключены");
            self.shutdown();
            return false;
        }
        // Back to waiting for the next X client
        self.watch(handle);
        true
    }

    /// Frees the display: X programs fail fast instead of hanging on a dead socket
    fn shutdown(&mut self) {
        *X_DISPLAY.lock().unwrap() = None;
        let _ = std::fs::remove_file(socket_path(self.number));
        let _ = std::fs::remove_file(lock_path(self.number));
    }
}

impl Drop for Xwayland {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl FeatherCompositor {
    pub fn start_xwayland(&mut self, update_activation_env: bool) {
        let socket = self.wayland_socket_name.clone();
        self.xwayland = Xwayland::new(&self.loop_handle, &socket, update_activation_env);
    }

    fn xwayland_wanted(&mut self) {
        let handle = self.loop_handle.clone();
        if let Some(x) = self.xwayland.as_mut().filter(|x| !x.running) {
            x.spawn(&handle);
        }
    }

    fn xwayland_exited(&mut self) {
        let handle = self.loop_handle.clone();
        if let Some(x) = self.xwayland.as_mut() {
            if !x.exited(&handle) {
                self.xwayland = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_locks_are_taken_over_and_live_ones_skipped() {
        // Only checks the lock rule on a private name; real /tmp/.X*-lock files are not touched.
        let dir = std::env::temp_dir().join(format!("feather-xlock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let live = dir.join("live");
        let stale = dir.join("stale");
        let odd = dir.join("odd");
        std::fs::write(&live, format!("{:>10}\n", std::process::id())).unwrap();
        std::fs::write(&stale, format!("{:>10}\n", u32::MAX - 1)).unwrap();
        std::fs::write(&odd, "not a pid").unwrap();
        assert!(lock_owner_alive(&live));
        assert!(!lock_owner_alive(&stale));
        assert!(lock_owner_alive(&odd), "unknown format is never taken over");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
