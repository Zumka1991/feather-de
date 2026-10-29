//! Установщик пакетов (`feather-installer`) — opens a .deb (from the browser, Files or the
//! command line), shows what it is and installs it with apt, asking for the administrator
//! password through polkit. Zero idle CPU: event-driven `calloop` + `wl_shm` + `tiny-skia`.

mod app;
mod package;

use app::{Action, App, HEIGHT, WIDTH};
use feather_ui::i18n::tr;
use std::time::Instant;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{channel, EventLoop, LoopHandle},
        calloop_wayland_source::WaylandSource,
        csd_frame::WindowState,
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

enum Msg {
    Appearance(feather_ui::scale::Appearance),
    Progress(package::Progress),
}

struct InstallerWin {
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
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<ThemedPointer>,
    pointer_pos: (f32, f32),
    serial: u32,
    ctrl: bool,
    tx: channel::Sender<Msg>,
    handle: LoopHandle<'static, InstallerWin>,
}

fn main() {
    // Started from an older Files or a script with SIGCHLD ignored: our own waits need it back
    feather_ui::process::default_child_signal();
    feather_ui::appearance::init();
    let started = Instant::now();
    let path = std::env::args().skip(1).find(|a| !a.starts_with("--")).map(|a| package::path_from_arg(&a));
    let app = match path {
        Some(path) => App::new(path),
        None => App::with(Default::default(), app::State::Invalid("Откройте пакет .deb из браузера или Файлов".into())),
    };

    let conn = Connection::connect_to_env().expect("Нет Wayland-композитора (WAYLAND_DISPLAY)");
    let (globals, queue) = registry_queue_init(&conn).expect("Реестр Wayland недоступен");
    let qh = queue.handle();
    let mut event_loop: EventLoop<InstallerWin> = EventLoop::try_new().expect("цикл событий");
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue).insert(handle.clone()).expect("источник Wayland");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let xdg = XdgShell::bind(&globals, &qh).expect("xdg_wm_base");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let surface = compositor.create_surface(&qh);
    let window = xdg.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_title(tr("Установка программы"));
    window.set_app_id("feather-installer");
    window.set_min_size(Some((WIDTH as u32, HEIGHT as u32)));
    window.set_max_size(Some((WIDTH as u32, HEIGHT as u32)));

    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    let fractional_manager: Option<WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
    let (viewport, fractional_object) = match (&viewporter, &fractional_manager) {
        (Some(v), Some(f)) => (Some(v.get_viewport(window.wl_surface(), &qh, ())), Some(f.get_fractional_scale(window.wl_surface(), &qh, ()))),
        _ => (None, None),
    };
    window.commit();

    let (tx, rx) = channel::channel::<Msg>();
    handle
        .insert_source(rx, |event, _, win| {
            if let channel::Event::Msg(msg) = event {
                match msg {
                    Msg::Appearance(appearance) => {
                        if feather_ui::appearance::sync(appearance) {
                            win.app.dirty = true;
                        }
                    }
                    Msg::Progress(progress) => win.app.progress(progress),
                }
            }
        })
        .expect("канал сообщений");

    let rgba = shm.formats().contains(&wl_shm::Format::Abgr8888);
    let mut win = InstallerWin {
        _appearance_watch: {
            let tx = tx.clone();
            feather_ui::appearance::watch(move |a| tx.send(Msg::Appearance(a)).is_ok()).ok()
        },
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(WIDTH as usize * HEIGHT as usize * 4, &shm).expect("пул wl_shm"),
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
        seat: None,
        keyboard: None,
        pointer: None,
        pointer_pos: (-1.0, -1.0),
        serial: 0,
        ctrl: false,
        tx,
        handle: handle.clone(),
    };

    let profile = std::env::var_os("FEATHER_PROFILE").is_some();
    let mut first_frame = true;
    while !win.exit {
        if event_loop.dispatch(None, &mut win).is_err() {
            break;
        }
        if win.app.dirty && win.draw(&qh) && profile && first_frame {
            first_frame = false;
            eprintln!("[feather-installer] первый кадр через {:.1} мс после запуска", started.elapsed().as_secs_f64() * 1e3);
        }
    }
}

impl InstallerWin {
    fn apply_action(&mut self, action: Action) {
        match action {
            Action::None | Action::Redraw => {}
            Action::Close => self.exit = true,
            Action::Minimize => self.window.set_minimized(),
            Action::DragWindow => {
                if let Some(seat) = &self.seat {
                    self.window.move_(seat, self.serial);
                }
            }
            Action::Install => {
                self.app.start_install();
                let tx = self.tx.clone();
                package::install(self.app.path.clone(), move |p| {
                    let _ = tx.send(Msg::Progress(p));
                });
            }
        }
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) -> bool {
        if !self.configured || self.frame_pending {
            return false;
        }
        let (w, h) = (WIDTH as i32, HEIGHT as i32);
        let scale = match (self.fractional, &self.viewport) {
            (Some(f), Some(_)) => f as f32,
            _ => self.scale.max(1) as f32,
        };
        feather_ui::scale::set_ui_scale(scale);
        let (pw, ph) = ((w as f32 * scale).round() as i32, (h as f32 * scale).round() as i32);
        let format = if self.rgba { wl_shm::Format::Abgr8888 } else { wl_shm::Format::Argb8888 };
        let Ok((buffer, canvas)) = self.pool.create_buffer(pw, ph, pw * 4, format) else {
            return false;
        };
        let canvas = &mut canvas[..(pw as usize * ph as usize * 4)];
        {
            let Some(mut pm) = tiny_skia::PixmapMut::from_bytes(canvas, pw as u32, ph as u32) else {
                return false;
            };
            self.app.draw(&mut pm);
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

    fn key(&mut self, event: KeyEvent) {
        if self.ctrl && matches!(event.keysym, Keysym::q | Keysym::Q | Keysym::w | Keysym::W) {
            if !self.app.installing() {
                self.exit = true;
            }
            return;
        }
        let enter = matches!(event.keysym, Keysym::Return | Keysym::KP_Enter);
        let escape = event.keysym == Keysym::Escape;
        let action = self.app.handle_key(enter, escape);
        self.apply_action(action);
    }
}

impl CompositorHandler for InstallerWin {
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

impl OutputHandler for InstallerWin {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for InstallerWin {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        // apt keeps running as root without us; the window stays until it finishes
        if !self.app.installing() {
            self.exit = true;
        }
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, configure: WindowConfigure, _: u32) {
        let focused = configure.state.contains(WindowState::ACTIVATED);
        if focused != self.app.focused {
            self.app.focused = focused;
            self.app.dirty = true;
        }
        if !self.configured {
            self.configured = true;
            self.app.dirty = true;
        }
    }
}

impl SeatHandler for InstallerWin {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        self.seat = Some(seat.clone());
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let keyboard = self.seat_state.get_keyboard_with_repeat(qh, &seat, None, self.handle.clone(), Box::new(|win, _, event| win.key(event)));
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

impl KeyboardHandler for InstallerWin {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, serial: u32, _: &[u32], _: &[Keysym]) {
        self.serial = serial;
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {
        self.ctrl = false;
    }
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, serial: u32, event: KeyEvent) {
        self.serial = serial;
        self.key(event);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, m: Modifiers, _: u32) {
        self.ctrl = m.ctrl || m.logo;
    }
}

impl PointerHandler for InstallerWin {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for event in events {
            if &event.surface != self.window.wl_surface() {
                continue;
            }
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            let action = match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.pointer_pos = (x, y);
                    self.app.handle_pointer_motion(x, y)
                }
                PointerEventKind::Leave { .. } => self.app.handle_pointer_motion(-1.0, -1.0),
                PointerEventKind::Press { button: 0x110, serial, .. } => {
                    self.serial = serial;
                    self.app.handle_press(x, y)
                }
                PointerEventKind::Release { button: 0x110, .. } => self.app.handle_release(x, y),
                _ => Action::None,
            };
            self.apply_action(action);
        }
    }
}

impl ShmHandler for InstallerWin {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(InstallerWin);
delegate_output!(InstallerWin);
delegate_shm!(InstallerWin);
delegate_seat!(InstallerWin);
delegate_keyboard!(InstallerWin);
delegate_pointer!(InstallerWin);
delegate_xdg_shell!(InstallerWin);
delegate_xdg_window!(InstallerWin);
delegate_registry!(InstallerWin);

impl ProvidesRegistryState for InstallerWin {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpFractionalScaleV1, ()> for InstallerWin {
    fn event(win: &mut Self, _: &WpFractionalScaleV1, event: wp_fractional_scale_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let scale = scale as f64 / 120.0;
            if win.fractional != Some(scale) {
                win.fractional = Some(scale);
                win.app.dirty = true;
            }
        }
    }
}

macro_rules! no_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for InstallerWin {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
no_events!(WpFractionalScaleManagerV1, WpViewporter, WpViewport);
