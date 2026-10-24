//! Idle deadlines are one-shot timers; bus work never runs on the compositor thread.
use crate::FeatherCompositor;
use smithay::{
    reexports::{
        calloop::{
            channel,
            timer::{TimeoutAction, Timer},
            RegistrationToken,
        },
        wayland_server::protocol::wl_surface::WlSurface,
    },
    wayland::{
        idle_inhibit::IdleInhibitHandler,
        idle_notify::{IdleNotifierHandler, IdleNotifierState},
    },
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Preferences {
    pub lock: u32,
    pub blank: u32,
    pub suspend: u32,
}
impl Preferences {
    pub const TIMES: [u32; 8] = [0, 1, 2, 5, 10, 15, 30, 60];
    pub fn next(value: u32) -> u32 {
        Self::TIMES
            .iter()
            .copied()
            .find(|m| *m > value)
            .unwrap_or(0)
    }
    pub fn label(value: u32) -> String {
        if value == 0 {
            "Никогда".into()
        } else {
            format!("Через {value} мин")
        }
    }
}
pub struct Power {
    pub preferences: Preferences,
    pub blanked: bool,
    pub ready: bool,
    pub sleeping: bool,
    idle_hint: bool,
    blank_attempted: bool,
    last_activity: Instant,
    suspended: bool,
    timer: Option<RegistrationToken>,
    inhibitors: Vec<WlSurface>,
    inhibited: bool,
    bus: Option<async_channel::Sender<Command>>,
}
impl Power {
    pub fn new(preferences: Preferences) -> Self {
        Self {
            preferences,
            blanked: false,
            ready: false,
            sleeping: false,
            idle_hint: false,
            blank_attempted: false,
            last_activity: Instant::now(),
            suspended: false,
            timer: None,
            inhibitors: Vec::new(),
            inhibited: false,
            bus: None,
        }
    }
    fn send(&self, command: Command) {
        if let Some(tx) = &self.bus {
            let _ = tx.try_send(command);
        }
    }
    pub fn locked_hint(&self, locked: bool) {
        self.send(Command::Locked(locked));
    }
    pub fn protected(&self) {
        self.send(Command::Protected);
    }
}
#[derive(Clone, Copy)]
enum Command {
    Locked(bool),
    Protected,
    Idle(bool),
    Suspend,
}
#[derive(Debug)]
enum Event {
    Ready,
    Lock,
    Sleep(bool),
    Error(String),
}

impl IdleNotifierHandler for FeatherCompositor {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier
    }
}
impl IdleInhibitHandler for FeatherCompositor {
    fn inhibit(&mut self, surface: WlSurface) {
        self.power.inhibitors.push(surface);
        self.sync_idle_inhibit();
    }
    fn uninhibit(&mut self, surface: WlSurface) {
        if let Some(i) = self.power.inhibitors.iter().position(|s| s == &surface) {
            self.power.inhibitors.remove(i);
        }
        self.sync_idle_inhibit();
    }
}
smithay::delegate_idle_notify!(FeatherCompositor);
smithay::delegate_idle_inhibit!(FeatherCompositor);

impl FeatherCompositor {
    pub fn sync_idle_inhibit(&mut self) {
        self.power
            .inhibitors
            .retain(|s| smithay::reexports::wayland_server::Resource::is_alive(s));
        let inhibited = !self.lock.active
            && self.power.inhibitors.iter().any(|s| {
                let mut root = s.clone();
                while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
                    root = parent;
                }
                self.windows
                    .iter()
                    .any(|w| w.is_visible() && w.wl_surface() == Some(&root))
            });
        self.idle_notifier.set_is_inhibited(inhibited);
        if self.power.inhibited != inhibited {
            self.power.inhibited = inhibited;
            // A video stopping starts a fresh idle interval; it does not suspend immediately.
            self.power.last_activity = Instant::now();
            self.rearm_idle();
        }
    }
    pub fn activity(&mut self) -> bool {
        self.idle_notifier.notify_activity(&self.seat);
        self.power.last_activity = Instant::now();
        self.power.suspended = false;
        self.power.blank_attempted = false;
        if self.power.idle_hint {
            self.power.idle_hint = false;
            self.power.send(Command::Idle(false));
        }
        let wake = self.power.blanked;
        if wake {
            self.wake_screen();
        }
        // Keep one timer per seat even during pointer motion: callback uses latest timestamp.
        if self.power.timer.is_none() {
            self.rearm_idle();
        }
        wake
    }
    pub fn rearm_idle(&mut self) {
        if let Some(token) = self.power.timer.take() {
            self.loop_handle.remove(token);
        }
        if self.power.inhibited {
            return;
        }
        if matches!(&self.backend, crate::backend::Backend::Drm(d) if !d.active) {
            return;
        }
        let p = self.power.preferences;
        let elapsed = self.power.last_activity.elapsed();
        let deadline = [
            (!self.lock.active, p.lock),
            (!self.power.blanked && !self.power.blank_attempted, p.blank),
            (!self.power.suspended, p.suspend),
        ]
        .into_iter()
        .filter(|(needed, m)| *needed && *m > 0)
        .map(|(_, m)| Duration::from_secs(m as u64 * 60))
        .min();
        let Some(deadline) = deadline else {
            return;
        };
        let duration = deadline
            .saturating_sub(elapsed)
            .max(Duration::from_millis(1));
        if let Ok(token) =
            self.loop_handle
                .insert_source(Timer::from_duration(duration), |_, _, st| {
                    st.power.timer = None;
                    st.idle_deadline();
                    TimeoutAction::Drop
                })
        {
            self.power.timer = Some(token);
        }
    }
    pub fn debug_power_check(&mut self) {
        assert!(!self.backend.is_drm() && !self.power.ready);
        self.power.preferences = Preferences {
            lock: 1,
            blank: 1,
            suspend: 1,
        };
        self.power.last_activity = Instant::now() - Duration::from_secs(61);
        self.idle_deadline();
        assert!(self.lock.active && self.power.blanked && self.power.suspended);
        assert!(self.keyboard.current_focus().is_none());
        assert!(self.activity());
        assert!(!self.power.blanked);
        assert!(self.lock.active);
        assert!(!self.activity());
        assert!(!self.power.suspended);
        self.power.preferences = Preferences::default();
        self.rearm_idle();
        eprintln!("[feather-lock-check] idle lock/blank/suspend, wake consumed, host untouched");
    }
    fn idle_deadline(&mut self) {
        self.sync_idle_inhibit();
        if self.power.inhibited {
            return;
        }
        let elapsed = self.power.last_activity.elapsed().as_secs();
        let p = self.power.preferences;
        if !self.power.idle_hint {
            self.power.idle_hint = true;
            self.power.send(Command::Idle(true));
        }
        if p.lock > 0 && elapsed >= p.lock as u64 * 60 {
            self.lock_session();
        }
        if p.blank > 0 && elapsed >= p.blank as u64 * 60 {
            self.blank_screen();
        }
        if p.suspend > 0 && elapsed >= p.suspend as u64 * 60 && !self.power.suspended {
            self.power.suspended = true;
            self.lock_session();
            if self.backend.is_drm() && self.power.ready {
                self.power.send(Command::Suspend);
            }
        }
        self.rearm_idle();
    }
    fn blank_screen(&mut self) {
        if self.power.blanked {
            return;
        }
        self.power.blank_attempted = true;
        self.lock_session();
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            if !d.active {
                return;
            }
            if let Err(e) = d.clear_all() {
                tracing::warn!("Гашение экрана: {e}");
                return;
            }
        }
        self.power.blanked = true;
        self.frame_pending = false;
        self.needs_redraw = true;
    }
    pub fn wake_screen(&mut self) {
        if !self.power.blanked {
            return;
        }
        self.power.blanked = false;
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            for h in &mut d.heads{h.output.reset_buffers();h.pending=false;h.protected=false;}
        }
        self.frame_pending = false;
        self.needs_redraw = true;
    }
    pub fn start_power(&mut self) {
        self.rearm_idle();
        // An ordinary nested test/window must never lock, inhibit or suspend its host session.
        if !self.backend.is_drm() {
            return;
        }
        let (tx, rx) = async_channel::bounded(32);
        let (events, source) = channel::channel();
        if self
            .loop_handle
            .insert_source(source, |event, _, st| {
                if let channel::Event::Msg(event) = event {
                    match event {
                        Event::Ready => {
                            st.power.ready = true;
                            st.power.send(Command::Idle(false));
                            st.power.locked_hint(st.lock.active);
                            st.mark_builtin_windows_dirty();
                        }
                        Event::Lock => st.lock_session(),
                        Event::Sleep(sleeping) => {
                            st.power.sleeping = sleeping;
                            if sleeping {
                                st.lock_sleep_barrier();
                            } else {
                                st.lock_sleep_barrier();
                                st.wake_screen();
                                st.activity();
                                st.needs_redraw = true;
                            }
                        }
                        Event::Error(e) => {
                            tracing::warn!("Энергосбережение: {e}");
                            let was_ready = st.power.ready;
                            st.power.ready = false;
                            if was_ready {
                                st.lock_sleep_barrier();
                            }
                            st.mark_builtin_windows_dirty();
                        }
                    }
                } else if st.power.ready {
                    st.power.ready = false;
                    st.lock_sleep_barrier();
                    st.mark_builtin_windows_dirty();
                }
            })
            .is_err()
        {
            return;
        }
        self.power.bus = Some(tx);
        let _ = std::thread::Builder::new()
            .name("feather-logind".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                if let Err(e) = futures_lite::future::block_on(logind(rx, &events)) {
                    let _ = events.send(Event::Error(e.to_string()));
                }
            });
    }
}

async fn logind(
    commands: async_channel::Receiver<Command>,
    events: &channel::Sender<Event>,
) -> Result<(), Box<dyn std::error::Error>> {
    use zbus::Connection;
    let conn = Connection::system().await?;
    logind_connected(conn, commands, events).await
}

async fn logind_connected(
    conn: zbus::Connection,
    commands: async_channel::Receiver<Command>,
    events: &channel::Sender<Event>,
) -> Result<(), Box<dyn std::error::Error>> {
    use futures_lite::{future, StreamExt};
    use zbus::{
        zvariant::{OwnedFd, OwnedObjectPath},
        Proxy,
    };
    let manager = Proxy::new(
        &conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let path: OwnedObjectPath = manager
        .call("GetSessionByPID", &(std::process::id(),))
        .await?;
    let session = Proxy::new(
        &conn,
        "org.freedesktop.login1",
        path,
        "org.freedesktop.login1.Session",
    )
    .await?;
    let mut sleep = manager.receive_signal("PrepareForSleep").await?;
    let mut lock = session.receive_signal("Lock").await?;
    let mut inhibitor: Option<OwnedFd> = Some(
        manager
            .call(
                "Inhibit",
                &("sleep", "Пёрышко", "Блокировка экрана перед сном", "delay"),
            )
            .await?,
    );
    let mut sleeping = false;
    events.send(Event::Ready)?;
    enum Next {
        Command(Result<Command, async_channel::RecvError>),
        Sleep(Option<zbus::Message>),
        Lock(Option<zbus::Message>),
    }
    loop {
        let next = future::race(
            async { Next::Command(commands.recv().await) },
            future::race(async { Next::Sleep(sleep.next().await) }, async {
                Next::Lock(lock.next().await)
            }),
        )
        .await;
        match next {
            Next::Lock(Some(_)) => {
                events.send(Event::Lock)?;
            }
            Next::Sleep(Some(msg)) => {
                let value: bool = msg.body().deserialize()?;
                sleeping = value;
                events.send(Event::Sleep(value))?;
                if !value {
                    inhibitor = Some(
                        manager
                            .call(
                                "Inhibit",
                                &("sleep", "Пёрышко", "Блокировка экрана перед сном", "delay"),
                            )
                            .await?,
                    );
                }
            }
            Next::Command(Ok(Command::Protected)) => {
                if sleeping {
                    inhibitor.take();
                }
            }
            Next::Command(Ok(Command::Locked(value))) => {
                let _: () = session.call("SetLockedHint", &(value,)).await?;
            }
            Next::Command(Ok(Command::Idle(value))) => {
                let _: () = session.call("SetIdleHint", &(value,)).await?;
            }
            Next::Command(Ok(Command::Suspend)) => {
                // Suspend may answer only after resume. Keep processing PrepareForSleep meanwhile.
                let manager = manager.clone();
                conn.executor()
                    .spawn(
                        async move {
                            if let Err(e) = manager.call::<_, _, ()>("Suspend", &(false,)).await {
                                tracing::warn!("Сон: {e}");
                            }
                        },
                        "feather-suspend",
                    )
                    .detach();
            }
            _ => break,
        }
    }
    drop(inhibitor);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn time_choices_are_bounded_and_wrap() {
        assert_eq!(Preferences::next(0), 1);
        assert_eq!(Preferences::next(7), 10);
        assert_eq!(Preferences::next(60), 0);
        assert_eq!(Preferences::label(0), "Никогда");
    }

    #[derive(Default)]
    struct Observed {
        peers: std::sync::Mutex<Vec<std::os::unix::net::UnixStream>>,
        locked: std::sync::atomic::AtomicBool,
        idle: std::sync::atomic::AtomicBool,
        suspends: std::sync::atomic::AtomicUsize,
    }
    struct FakeManager(std::sync::Arc<Observed>);
    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl FakeManager {
        #[zbus(name = "GetSessionByPID")]
        fn get_session_by_pid(&self, _pid: u32) -> zbus::zvariant::OwnedObjectPath {
            "/org/freedesktop/login1/session/self".try_into().unwrap()
        }
        fn inhibit(
            &self,
            what: &str,
            _who: &str,
            _why: &str,
            mode: &str,
        ) -> zbus::zvariant::OwnedFd {
            assert_eq!((what, mode), ("sleep", "delay"));
            let (peer, fd) = std::os::unix::net::UnixStream::pair().unwrap();
            peer.set_nonblocking(true).unwrap();
            self.0.peers.lock().unwrap().push(peer);
            zbus::zvariant::OwnedFd::from(std::os::fd::OwnedFd::from(fd))
        }
        fn suspend(&self, interactive: bool) {
            assert!(!interactive);
            self.0
                .suspends
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    struct FakeSession(std::sync::Arc<Observed>);
    #[zbus::interface(name = "org.freedesktop.login1.Session")]
    impl FakeSession {
        fn set_locked_hint(&self, value: bool) {
            self.0
                .locked
                .store(value, std::sync::atomic::Ordering::SeqCst);
        }
        fn set_idle_hint(&self, value: bool) {
            self.0
                .idle
                .store(value, std::sync::atomic::Ordering::SeqCst);
        }
    }
    struct Bus(std::process::Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    #[test]
    fn private_logind_waits_for_protected_frame_and_reacquires_inhibitor_on_resume() {
        use futures_lite::future::block_on;
        use std::{
            io::{BufRead, BufReader, Read},
            sync::{atomic::Ordering, Arc},
        };
        // This daemon is owned by this test. Neither the session bus nor system bus is used.
        let mut daemon = Bus(std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap());
        let mut address = String::new();
        BufReader::new(daemon.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let observed = Arc::new(Observed::default());
        let server = block_on(async {
            zbus::connection::Builder::address(address.trim())
                .unwrap()
                .name("org.freedesktop.login1")
                .unwrap()
                .serve_at("/org/freedesktop/login1", FakeManager(observed.clone()))
                .unwrap()
                .serve_at(
                    "/org/freedesktop/login1/session/self",
                    FakeSession(observed.clone()),
                )
                .unwrap()
                .build()
                .await
                .unwrap()
        });
        let conn = block_on(
            zbus::connection::Builder::address(address.trim())
                .unwrap()
                .build(),
        )
        .unwrap();
        let (commands, rx) = async_channel::bounded(32);
        let (events, source) = channel::channel();
        let mut event_loop =
            smithay::reexports::calloop::EventLoop::<Vec<Event>>::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(source, |event, _, state| {
                if let channel::Event::Msg(e) = event {
                    state.push(e)
                }
            })
            .unwrap();
        let worker = std::thread::spawn(move || {
            block_on(logind_connected(conn, rx, &events)).map_err(|e| {
                let text = e.to_string();
                let _ = events.send(Event::Error(text.clone()));
                text
            })
        });
        let mut received = Vec::new();
        fn wait(
            event_loop: &mut smithay::reexports::calloop::EventLoop<Vec<Event>>,
            received: &mut Vec<Event>,
            predicate: impl Fn(&[Event]) -> bool,
        ) {
            let until = Instant::now() + Duration::from_secs(5);
            while !predicate(received) {
                assert!(
                    Instant::now() < until,
                    "private bus event timed out: {received:?}"
                );
                event_loop
                    .dispatch(Duration::from_millis(20), received)
                    .unwrap();
            }
        }
        wait(&mut event_loop, &mut received, |events| {
            events.iter().any(|e| matches!(e, Event::Ready))
        });
        block_on(server.emit_signal(
            None::<&str>,
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "PrepareForSleep",
            &(true,),
        ))
        .unwrap();
        wait(&mut event_loop, &mut received, |events| {
            events.iter().any(|e| matches!(e, Event::Sleep(true)))
        });
        let mut byte = [0u8; 1];
        assert_eq!(
            observed.peers.lock().unwrap()[0]
                .read(&mut byte)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        commands.send_blocking(Command::Protected).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if observed.peers.lock().unwrap()[0].read(&mut byte).ok() == Some(0) {
                break;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        block_on(server.emit_signal(
            None::<&str>,
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "PrepareForSleep",
            &(false,),
        ))
        .unwrap();
        wait(&mut event_loop, &mut received, |events| {
            events.iter().any(|e| matches!(e, Event::Sleep(false)))
        });
        commands.send_blocking(Command::Locked(true)).unwrap();
        commands.send_blocking(Command::Idle(true)).unwrap();
        commands.send_blocking(Command::Suspend).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while observed.peers.lock().unwrap().len() < 2
            || !observed.locked.load(Ordering::SeqCst)
            || !observed.idle.load(Ordering::SeqCst)
            || observed.suspends.load(Ordering::SeqCst) != 1
        {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            observed.peers.lock().unwrap()[1]
                .read(&mut byte)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        block_on(server.emit_signal(
            None::<&str>,
            "/org/freedesktop/login1/session/self",
            "org.freedesktop.login1.Session",
            "Lock",
            &(),
        ))
        .unwrap();
        wait(&mut event_loop, &mut received, |events| {
            events.iter().any(|e| matches!(e, Event::Lock))
        });
        drop(commands);
        assert!(worker.join().unwrap().is_ok());
    }
}
