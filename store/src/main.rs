//! Магазин (`feather-store`) — Feather's catalog of programs: the AppStream metadata apt
//! downloads, searched and shown in Feather's style, installed and removed through
//! PackageKit. Idle costs nothing: only input and the results of worker threads (catalog,
//! icons, PackageKit) wake it up.

mod app;
mod catalog;
mod packagekit;

use feather_ui::text::FontSystem;
use std::time::Instant;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{channel, EventLoop, LoopHandle},
        calloop_wayland_source::WaylandSource,
        csd_frame::{CursorIcon, WindowState},
        protocols::wp::{
            fractional_scale::v1::client::{
                wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
                wp_fractional_scale_v1::{self, WpFractionalScaleV1},
            },
            viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        xdg::{
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
            XdgShell, XdgSurface,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, Dispatch, Proxy, QueueHandle,
};

use app::{App, Effect};

enum Msg {
    Appearance(feather_ui::scale::Appearance),
    Catalog(catalog::Catalog),
    Installed(std::collections::HashSet<String>),
    Icons(Vec<(usize, Option<tiny_skia::Pixmap>)>),
    Task(packagekit::Event),
}

struct Store {
    _appearance_watch: Option<feather_ui::appearance::Watcher>,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    window: Window,
    pool: SlotPool,
    rgba: bool,
    scale: i32,
    fractional: Option<f64>,
    viewport: Option<WpViewport>,
    _fractional_object: Option<WpFractionalScaleV1>,
    configured: bool,
    frame_pending: bool,
    exit: bool,
    app: App,
    /// The catalog arrived: focus changes may refresh what dpkg has installed
    loaded: bool,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<ThemedPointer>,
    cursor: Option<app::Cursor>,
    serial: u32,
    mods: Modifiers,
    tx: channel::Sender<Msg>,
    handle: LoopHandle<'static, Store>,
    conn: Connection,
    pointer_pos: (f32, f32),
}

fn main() {
    // Started from an older Files or a script with SIGCHLD ignored: our own waits need it back
    feather_ui::process::default_child_signal();
    feather_ui::appearance::init();
    let started = Instant::now();
    let mut app = App::new(FontSystem::new());
    if let Some(id) = std::env::args().skip(1).find(|a| !a.starts_with("--")) {
        app.open_later(&id);
    }

    let conn = Connection::connect_to_env().expect("Нет Wayland-композитора (WAYLAND_DISPLAY)");
    let (globals, queue) = registry_queue_init(&conn).expect("Реестр Wayland недоступен");
    let qh = queue.handle();
    let mut event_loop: EventLoop<Store> = EventLoop::try_new().expect("цикл событий");
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue).insert(handle.clone()).expect("источник Wayland");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let xdg = XdgShell::bind(&globals, &qh).expect("xdg_wm_base");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let surface = compositor.create_surface(&qh);
    let window = xdg.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_title(feather_ui::i18n::tr("Магазин"));
    window.set_app_id("feather-store");
    window.set_min_size(Some((app::MIN_W as u32, app::MIN_H as u32)));
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    let fractional_manager: Option<WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
    let (viewport, fractional_object) = match (&viewporter, &fractional_manager) {
        (Some(v), Some(f)) => (Some(v.get_viewport(window.wl_surface(), &qh, ())), Some(f.get_fractional_scale(window.wl_surface(), &qh, ()))),
        _ => (None, None),
    };
    window.commit();

    let (tx, rx) = channel::channel::<Msg>();
    handle
        .insert_source(rx, |event, _, store| {
            if let channel::Event::Msg(msg) = event {
                match msg {
                    Msg::Appearance(appearance) => {
                        if feather_ui::appearance::sync(appearance) {
                            store.app.dirty = true;
                        }
                    }
                    Msg::Catalog(catalog) => {
                        store.loaded = true;
                        store.app.loaded(catalog);
                    }
                    Msg::Installed(installed) => store.app.installed_changed(installed),
                    Msg::Icons(icons) => store.app.icons_loaded(icons),
                    Msg::Task(event) => store.app.task_event(event),
                }
            }
        })
        .expect("канал результатов");
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Catalog(catalog::load()));
        });
    }

    let rgba = shm.formats().contains(&wl_shm::Format::Abgr8888);
    let mut store = Store {
        _appearance_watch: {
            let tx = tx.clone();
            feather_ui::appearance::watch(move |a| tx.send(Msg::Appearance(a)).is_ok()).ok()
        },
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(app::DEFAULT_W as usize * app::DEFAULT_H as usize * 4, &shm).expect("пул wl_shm"),
        compositor,
        shm,
        window,
        rgba,
        scale: 1,
        fractional: None,
        viewport,
        _fractional_object: fractional_object,
        configured: false,
        frame_pending: false,
        exit: false,
        app,
        loaded: false,
        seat: None,
        keyboard: None,
        pointer: None,
        cursor: None,
        serial: 0,
        mods: Modifiers::default(),
        tx,
        handle: handle.clone(),
        conn,
        pointer_pos: (0.0, 0.0),
    };

    let profile = std::env::var_os("FEATHER_PROFILE").is_some();
    let mut first_frame = true;
    while !store.exit {
        if event_loop.dispatch(None, &mut store).is_err() {
            break;
        }
        if store.app.dirty {
            let t = Instant::now();
            if store.draw(&qh) && profile {
                if first_frame {
                    eprintln!("[feather-store] первый кадр через {:.1} мс после запуска", started.elapsed().as_secs_f64() * 1e3);
                    first_frame = false;
                }
                eprintln!("[feather-store] кадр {:.2} мс", t.elapsed().as_secs_f64() * 1e3);
            }
            store.load_icons();
        }
    }
}

impl Store {
    fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Close => self.exit = true,
                Effect::Minimize => self.window.set_minimized(),
                Effect::ToggleMaximize => {
                    if self.app.maximized {
                        self.window.unset_maximized()
                    } else {
                        self.window.set_maximized()
                    }
                }
                Effect::Move => {
                    if let Some(seat) = &self.seat {
                        self.window.move_(seat, self.serial);
                    }
                }
                Effect::Resize(edges) => {
                    use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge as E;
                    let edge = match edges {
                        1 => E::Top,
                        2 => E::Bottom,
                        4 => E::Left,
                        8 => E::Right,
                        5 => E::TopLeft,
                        9 => E::TopRight,
                        6 => E::BottomLeft,
                        10 => E::BottomRight,
                        _ => E::None,
                    };
                    if let Some(seat) = &self.seat {
                        self.window.resize(seat, self.serial, edge);
                    }
                }
                Effect::Run(op, package, confirmed) => {
                    let tx = self.tx.clone();
                    packagekit::run(op, package, confirmed, move |event| {
                        let _ = tx.send(Msg::Task(event));
                    });
                }
                Effect::Launch(command) => catalog::launch(&command),
                Effect::OpenUrl(url) => catalog::open_url(&url),
            }
        }
    }

    /// Icons the last frame showed without having them: decoded on a worker thread
    fn load_icons(&mut self) {
        let wanted = self.app.take_icon_queue();
        if wanted.is_empty() {
            return;
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let icons = wanted.into_iter().map(|(i, path)| (i, tiny_skia::Pixmap::load_png(path).ok())).collect();
            let _ = tx.send(Msg::Icons(icons));
        });
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) -> bool {
        if !self.configured || self.frame_pending {
            return false;
        }
        let (w, h) = (self.app.w as i32, self.app.h as i32);
        let scale = match (self.fractional, &self.viewport) {
            (Some(f), Some(_)) => f as f32,
            _ => self.scale.max(1) as f32,
        };
        feather_ui::scale::set_ui_scale(scale);
        let (pw, ph) = ((w as f32 * scale).round() as i32, (h as f32 * scale).round() as i32);
        let format = if self.rgba { wl_shm::Format::Abgr8888 } else { wl_shm::Format::Argb8888 };
        let Ok((buffer, canvas)) = self.pool.create_buffer(pw, ph, pw * 4, format) else { return false };
        let canvas = &mut canvas[..(pw as usize * ph as usize * 4)];
        {
            let Some(mut pm) = tiny_skia::PixmapMut::from_bytes(canvas, pw as u32, ph as u32) else { return false };
            self.app.paint(&mut pm);
        }
        if !self.rgba {
            for px in canvas.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        let surface = self.window.wl_surface();
        match &self.viewport {
            Some(viewport) if self.fractional.is_some() => {
                surface.set_buffer_scale(1);
                viewport.set_destination(w, h);
            }
            _ => surface.set_buffer_scale(scale as i32),
        }
        surface.damage_buffer(0, 0, pw, ph);
        surface.frame(qh, surface.clone());
        self.window.set_window_geometry(0, 0, w as u32, h as u32);
        if buffer.attach_to(surface).is_err() {
            return false;
        }
        self.window.commit();
        self.frame_pending = true;
        true
    }

    fn update_cursor(&mut self) {
        let (x, y) = self.pointer_pos;
        let wanted = self.app.cursor_at(x, y);
        if self.cursor == Some(wanted) {
            return;
        }
        self.cursor = Some(wanted);
        let icon = match wanted {
            app::Cursor::Default => CursorIcon::Default,
            app::Cursor::Text => CursorIcon::Text,
            app::Cursor::Resize(e) => match e {
                1 => CursorIcon::NResize,
                2 => CursorIcon::SResize,
                4 => CursorIcon::WResize,
                8 => CursorIcon::EResize,
                5 => CursorIcon::NwResize,
                9 => CursorIcon::NeResize,
                6 => CursorIcon::SwResize,
                10 => CursorIcon::SeResize,
                _ => CursorIcon::Default,
            },
        };
        if let Some(p) = &self.pointer {
            let _ = p.set_cursor(&self.conn, icon);
        }
    }

    fn key(&mut self, event: KeyEvent) {
        let m = self.mods;
        let effects = self.app.key(event.keysym.raw(), event.raw_code, event.utf8.as_deref(), m.ctrl || m.logo, m.shift, m.alt);
        self.apply(effects);
    }
}

impl CompositorHandler for Store {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, factor: i32) {
        if factor != self.scale {
            self.scale = factor;
            self.app.dirty = true;
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Store {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for Store {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        // A running installation goes on in PackageKit without the window
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, configure: WindowConfigure, _: u32) {
        let (w, h) = (
            configure.new_size.0.map_or(self.app.w, |v| v.get() as f32),
            configure.new_size.1.map_or(self.app.h, |v| v.get() as f32),
        );
        self.app.maximized = configure.state.contains(WindowState::MAXIMIZED) || configure.state.contains(WindowState::FULLSCREEN);
        let focused = configure.state.contains(WindowState::ACTIVATED);
        if focused != self.app.focused {
            self.app.focused = focused;
            self.app.dirty = true;
            // Back from elsewhere (a terminal, the .deb installer): dpkg may know more now
            if focused && self.loaded && !self.app.busy() {
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::Installed(catalog::installed_packages()));
                });
            }
        }
        self.app.resize(w, h);
        if !self.configured {
            self.configured = true;
            self.app.dirty = true;
        }
    }
}

impl SeatHandler for Store {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        self.seat = Some(seat.clone());
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let keyboard = self.seat_state.get_keyboard_with_repeat(qh, &seat, None, self.handle.clone(), Box::new(|store, _, event| store.key(event)));
            self.keyboard = keyboard.ok();
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            let surface = self.compositor.create_surface(qh);
            self.pointer = self.seat_state.get_pointer_with_theme(qh, &seat, self.shm.wl_shm(), surface, ThemeSpec::default()).ok();
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if capability == Capability::Pointer {
            self.pointer = None;
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for Store {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, serial: u32, _: &[u32], _: &[Keysym]) {
        self.serial = serial;
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {
        self.mods = Modifiers::default();
    }
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, serial: u32, event: KeyEvent) {
        self.serial = serial;
        self.key(event);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, m: Modifiers, _: u32) {
        self.mods = m;
    }
}

impl PointerHandler for Store {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for event in events {
            if &event.surface != self.window.wl_surface() {
                continue;
            }
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            self.pointer_pos = (x, y);
            let effects = match event.kind {
                PointerEventKind::Enter { .. } => {
                    self.cursor = None;
                    self.app.motion(x, y);
                    Vec::new()
                }
                PointerEventKind::Leave { .. } => {
                    self.app.leave();
                    Vec::new()
                }
                PointerEventKind::Motion { .. } => {
                    self.app.motion(x, y);
                    Vec::new()
                }
                PointerEventKind::Press { button, serial, .. } => {
                    self.serial = serial;
                    self.app.press(button, x, y, self.mods.shift)
                }
                PointerEventKind::Release { button, serial, .. } => {
                    self.serial = serial;
                    self.app.release(button, x, y)
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let dy = if vertical.discrete != 0 { vertical.discrete as f32 * 56.0 } else { vertical.absolute as f32 };
                    self.app.scroll(dy, x);
                    Vec::new()
                }
            };
            self.apply(effects);
        }
        self.update_cursor();
    }
}

impl ShmHandler for Store {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Store);
delegate_output!(Store);
delegate_shm!(Store);
delegate_seat!(Store);
delegate_keyboard!(Store);
delegate_pointer!(Store);
delegate_xdg_shell!(Store);
delegate_xdg_window!(Store);
delegate_registry!(Store);

impl ProvidesRegistryState for Store {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpFractionalScaleV1, ()> for Store {
    fn event(store: &mut Self, _: &WpFractionalScaleV1, event: wp_fractional_scale_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let scale = scale as f64 / 120.0;
            if store.fractional != Some(scale) {
                store.fractional = Some(scale);
                store.app.dirty = true;
            }
        }
    }
}

macro_rules! no_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for Store {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
no_events!(WpFractionalScaleManagerV1, WpViewporter, WpViewport);
