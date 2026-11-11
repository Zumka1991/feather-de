//! Диспетчер задач (`feather-monitor`) — processes and resource graphs, opened by Ctrl+Alt+Del.
//! /proc is read on a background thread once a second only while the window exists; the
//! event loop just paints the snapshots it receives.

mod app;
mod sample;

use app::{Action, App};
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
    Snapshot(Box<sample::Snapshot>),
}

struct MonitorWin {
    _appearance_watch: Option<feather_ui::appearance::Watcher>,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    window: Window,
    pool: SlotPool,
    /// Real damage of each frame instead of the whole buffer
    diff: feather_ui::draw::FrameDiff,
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
    serial: u32,
    ctrl: bool,
    handle: LoopHandle<'static, MonitorWin>,
}

fn main() {
    feather_ui::process::default_child_signal();
    feather_ui::appearance::init();
    let started = Instant::now();
    let performance = std::env::args().any(|a| a == "--performance");

    let conn = Connection::connect_to_env().expect("Нет Wayland-композитора (WAYLAND_DISPLAY)");
    let (globals, queue) = registry_queue_init(&conn).expect("Реестр Wayland недоступен");
    let qh = queue.handle();
    let mut event_loop: EventLoop<MonitorWin> = EventLoop::try_new().expect("цикл событий");
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue)
        .insert(handle.clone())
        .expect("источник Wayland");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let xdg = XdgShell::bind(&globals, &qh).expect("xdg_wm_base");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let surface = compositor.create_surface(&qh);
    let window = xdg.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_title(tr("Мониторинг системы"));
    window.set_app_id("feather-monitor");
    window.set_min_size(Some((640, 420)));

    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    let fractional_manager: Option<WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
    let (viewport, fractional_object) = match (&viewporter, &fractional_manager) {
        (Some(v), Some(f)) => (
            Some(v.get_viewport(window.wl_surface(), &qh, ())),
            Some(f.get_fractional_scale(window.wl_surface(), &qh, ())),
        ),
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
                    Msg::Snapshot(snap) => win.app.update(*snap),
                }
            }
        })
        .expect("канал сообщений");
    {
        let tx = tx.clone();
        sample::spawn(move |snap| tx.send(Msg::Snapshot(Box::new(snap))).is_ok());
    }

    let mut app = App::new();
    if performance {
        app.tab = app::Tab::Performance;
    }
    let rgba = shm.formats().contains(&wl_shm::Format::Abgr8888);
    let mut win = MonitorWin {
        _appearance_watch: feather_ui::appearance::watch(move |a| tx.send(Msg::Appearance(a)).is_ok()).ok(),
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(860 * 600 * 4, &shm).expect("пул wl_shm"),
        diff: Default::default(),
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
        serial: 0,
        ctrl: false,
        handle: handle.clone(),
    };

    let profile = std::env::var_os("FEATHER_PROFILE").is_some();
    let mut first_frame = true;
    while !win.exit {
        if event_loop.dispatch(None, &mut win).is_err() {
            break;
        }
        if win.app.dirty {
            let t0 = Instant::now();
            if win.draw(&qh) && profile {
                if first_frame {
                    first_frame = false;
                    eprintln!(
                        "[feather-monitor] первый кадр через {:.1} мс после запуска",
                        started.elapsed().as_secs_f64() * 1e3
                    );
                }
                eprintln!("[feather-monitor] кадр {:.2} мс", t0.elapsed().as_secs_f64() * 1e3);
            }
        }
    }
}

impl MonitorWin {
    fn apply(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Close => self.exit = true,
            Action::Minimize => self.window.set_minimized(),
            Action::ToggleMaximize => {
                if self.app.maximized {
                    self.window.unset_maximized();
                } else {
                    self.window.set_maximized();
                }
            }
            Action::Drag => {
                if let Some(seat) = &self.seat {
                    self.window.move_(seat, self.serial);
                }
            }
        }
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) -> bool {
        if !self.configured || self.frame_pending {
            return false;
        }
        let (w, h) = (self.app.width as i32, self.app.height as i32);
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
        let damage = self.diff.damage_rects(canvas, pw, ph);
        let surface = self.window.wl_surface();
        match &self.viewport {
            Some(viewport) if self.fractional.is_some() => {
                surface.set_buffer_scale(1);
                viewport.set_destination(w, h);
            }
            _ => surface.set_buffer_scale(scale as i32),
        }
        for (dx, dy, dw, dh) in damage {
            surface.damage_buffer(dx, dy, dw, dh);
        }
        surface.frame(qh, surface.clone());
        self.window.set_window_geometry(0, 0, w as u32, h as u32);
        if buffer.attach_to(surface).is_err() {
            self.diff.reset();
            return false;
        }
        self.window.commit();
        self.frame_pending = true;
        true
    }

    fn key(&mut self, event: KeyEvent) {
        let action = self.app.handle_key(event.keysym.raw(), event.utf8.as_deref(), self.ctrl);
        self.apply(action);
    }
}

impl CompositorHandler for MonitorWin {
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

impl OutputHandler for MonitorWin {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for MonitorWin {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, configure: WindowConfigure, _: u32) {
        let w = configure.new_size.0.map_or(self.app.width as u32, |v| v.get());
        let h = configure.new_size.1.map_or(self.app.height as u32, |v| v.get());
        self.app.no_buttons = configure.state.contains(WindowState::TILED);
        let maximized = configure.state.contains(WindowState::MAXIMIZED) || configure.state.contains(WindowState::FULLSCREEN);
        let focused = configure.state.contains(WindowState::ACTIVATED);
        if focused != self.app.focused || maximized != self.app.maximized {
            self.app.focused = focused;
            self.app.maximized = maximized;
            self.app.dirty = true;
        }
        self.app.set_size(w, h);
        if !self.configured {
            self.configured = true;
            self.app.dirty = true;
        }
    }
}

impl SeatHandler for MonitorWin {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        self.seat = Some(seat.clone());
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let keyboard = self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.handle.clone(),
                Box::new(|win, _, event| win.key(event)),
            );
            self.keyboard = keyboard.ok();
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            let surface = self.compositor.create_surface(qh);
            self.pointer = self
                .seat_state
                .get_pointer_with_theme(qh, &seat, self.shm.wl_shm(), surface, ThemeSpec::default())
                .ok();
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

impl KeyboardHandler for MonitorWin {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, serial: u32, _: &[u32], _: &[Keysym]) {
        self.serial = serial;
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {
        self.ctrl = false;
        if self.app.shift {
            self.app.shift = false;
            self.app.dirty = true;
        }
    }
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, serial: u32, event: KeyEvent) {
        self.serial = serial;
        self.key(event);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, m: Modifiers, _: u32) {
        self.ctrl = m.ctrl || m.logo;
        // Shift turns «Снять задачу» into an immediate kill, so the button label follows it
        if m.shift != self.app.shift {
            self.app.shift = m.shift;
            self.app.dirty = true;
        }
    }
}

impl PointerHandler for MonitorWin {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for event in events {
            if &event.surface != self.window.wl_surface() {
                continue;
            }
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            let action = match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.app.handle_motion(x, y);
                    Action::None
                }
                PointerEventKind::Leave { .. } => {
                    self.app.handle_motion(-1.0, -1.0);
                    Action::None
                }
                PointerEventKind::Press { button, serial, .. } => {
                    self.serial = serial;
                    if button == 0x110 {
                        self.app.handle_click(x, y)
                    } else {
                        Action::None
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let dy = if vertical.absolute != 0.0 {
                        vertical.absolute as f32
                    } else {
                        vertical.discrete as f32 * 48.0
                    };
                    self.app.handle_scroll(dy);
                    Action::None
                }
                PointerEventKind::Release { .. } => Action::None,
            };
            self.apply(action);
        }
    }
}

impl ShmHandler for MonitorWin {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(MonitorWin);
delegate_output!(MonitorWin);
delegate_shm!(MonitorWin);
delegate_seat!(MonitorWin);
delegate_keyboard!(MonitorWin);
delegate_pointer!(MonitorWin);
delegate_xdg_shell!(MonitorWin);
delegate_xdg_window!(MonitorWin);
delegate_registry!(MonitorWin);

impl ProvidesRegistryState for MonitorWin {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpFractionalScaleV1, ()> for MonitorWin {
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
        impl Dispatch<$proxy, ()> for MonitorWin {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
no_events!(WpFractionalScaleManagerV1, WpViewporter, WpViewport);
