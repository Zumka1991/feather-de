//! Калькулятор (`feather-calc`) — native Wayland calculator in macOS Tahoe style.
//! Zero idle CPU: event-driven `calloop` + `wl_shm` shared-memory rendering with `tiny-skia`.

mod app;
mod theme;

use app::{Action, App, CalcMode, HISTORY_W};
use feather_ui::i18n::tr;
use std::io::Read;
use std::time::Instant;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    data_device_manager::{
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::{CopyPasteSource, DataSourceHandler},
        DataDeviceManagerState, WritePipe,
    },
    delegate_compositor, delegate_data_device, delegate_keyboard, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
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
    protocol::{
        wl_data_device::WlDataDevice,
        wl_data_device_manager::DndAction,
        wl_data_source::WlDataSource,
        wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface,
    },
    Connection, Dispatch, Proxy, QueueHandle,
};

const TEXT_TYPES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "TEXT",
    "STRING",
];

enum Msg {
    Appearance(feather_ui::scale::Appearance),
    Paste(String),
}

struct CalcWin {
    _appearance_watch: Option<feather_ui::appearance::Watcher>,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    data_devices: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    copy_source: Option<(CopyPasteSource, String)>,
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
    serial: u32,
    ctrl: bool,
    tx: channel::Sender<Msg>,
    handle: LoopHandle<'static, CalcWin>,
    qh: QueueHandle<CalcWin>,
}

fn main() {
    feather_ui::appearance::init();
    let started = Instant::now();
    let demo = std::env::args().any(|a| a == "--demo");

    let conn = Connection::connect_to_env().expect("Нет Wayland-композитора (WAYLAND_DISPLAY)");
    let (globals, queue) = registry_queue_init(&conn).expect("Реестр Wayland недоступен");
    let qh = queue.handle();
    let mut event_loop: EventLoop<CalcWin> = EventLoop::try_new().expect("цикл событий");
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue)
        .insert(handle.clone())
        .expect("источник Wayland");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let xdg = XdgShell::bind(&globals, &qh).expect("xdg_wm_base");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let surface = compositor.create_surface(&qh);
    let window = xdg.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_title(tr("Калькулятор"));
    window.set_app_id("feather-calc");
    window.set_min_size(Some((320, 440)));

    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    let fractional_manager: Option<WpFractionalScaleManagerV1> =
        globals.bind(&qh, 1..=1, ()).ok();
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
                    Msg::Paste(text) => {
                        win.app.paste_text(&text);
                    }
                }
            }
        })
        .expect("канал сообщений");

    let rgba = shm.formats().contains(&wl_shm::Format::Abgr8888);
    let mut win = CalcWin {
        _appearance_watch: {
            let tx = tx.clone();
            feather_ui::appearance::watch(move |a| tx.send(Msg::Appearance(a)).is_ok()).ok()
        },
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        data_devices: DataDeviceManagerState::bind(&globals, &qh).ok(),
        data_device: None,
        copy_source: None,
        pool: SlotPool::new(720 * 520 * 4, &shm).expect("пул wl_shm"),
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
        app: App::new(demo),
        seat: None,
        keyboard: None,
        pointer: None,
        serial: 0,
        ctrl: false,
        tx,
        handle: handle.clone(),
        qh: qh.clone(),
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
                        "[feather-calc] первый кадр через {:.1} мс после запуска",
                        started.elapsed().as_secs_f64() * 1e3
                    );
                }
                eprintln!(
                    "[feather-calc] кадр {:.2} мс",
                    t0.elapsed().as_secs_f64() * 1e3
                );
            }
        }
    }
}

impl CalcWin {
    fn ensure_mode_geometry(&mut self) {
        let base_w = match self.app.mode {
            CalcMode::Basic => 360.0,
            CalcMode::Scientific => 580.0,
            CalcMode::Programmer => 500.0,
        };
        let extra = if self.app.show_history { HISTORY_W } else { 0.0 };
        let target_w = base_w + extra;
        let target_h = if self.app.mode == CalcMode::Programmer {
            540.0
        } else {
            500.0
        };
        if !self.app.maximized {
            self.app.width = target_w;
            self.app.height = target_h;
        }
    }

    fn apply_action(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Redraw => {
                self.ensure_mode_geometry();
                self.app.dirty = true;
            }
            Action::CloseWindow => self.exit = true,
            Action::MinimizeWindow => self.window.set_minimized(),
            Action::ToggleMaximize => {
                if self.app.maximized {
                    self.window.unset_maximized();
                } else {
                    self.window.set_maximized();
                }
            }
            Action::DragWindow => {
                if let Some(seat) = &self.seat {
                    self.window.move_(seat, self.serial);
                }
            }
            Action::Copy(text) => {
                if let (Some(mgr), Some(dev)) = (&self.data_devices, &self.data_device) {
                    let src = mgr.create_copy_paste_source(&self.qh, TEXT_TYPES);
                    src.set_selection(dev, self.serial);
                    self.copy_source = Some((src, text));
                }
            }
        }
    }

    fn paste_from_selection(&mut self) {
        if let Some((_, text)) = &self.copy_source {
            let text = text.clone();
            self.app.paste_text(&text);
            return;
        }
        let Some(offer) = self
            .data_device
            .as_ref()
            .and_then(|d| d.data().selection_offer())
        else {
            return;
        };
        let Some(mime) = offer.with_mime_types(|types| {
            TEXT_TYPES
                .iter()
                .find(|t| types.iter().any(|m| m == *t))
                .map(|t| t.to_string())
        }) else {
            return;
        };
        let Ok(pipe) = offer.receive(mime) else {
            return;
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut reader = std::fs::File::from(std::os::fd::OwnedFd::from(pipe));
            if reader.by_ref().take(64 * 1024).read_to_end(&mut bytes).is_ok() {
                let _ = tx.send(Msg::Paste(String::from_utf8_lossy(&bytes).into_owned()));
            }
        });
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
        theme::set_ui_scale(scale);
        let (pw, ph) = (
            (w as f32 * scale).round() as i32,
            (h as f32 * scale).round() as i32,
        );
        let format = if self.rgba {
            wl_shm::Format::Abgr8888
        } else {
            wl_shm::Format::Argb8888
        };
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
        if self.ctrl && matches!(event.keysym, Keysym::v | Keysym::V) {
            self.paste_from_selection();
            return;
        }
        if self.ctrl && matches!(event.keysym, Keysym::q | Keysym::Q | Keysym::w | Keysym::W) {
            self.exit = true;
            return;
        }
        let action = self
            .app
            .handle_key(event.keysym.raw(), event.utf8.as_deref(), self.ctrl);
        self.apply_action(action);
    }
}

impl CompositorHandler for CalcWin {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if factor != self.scale {
            self.scale = factor;
            self.app.dirty = true;
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for CalcWin {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for CalcWin {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        let (w, h) = (
            configure
                .new_size
                .0
                .map_or(self.app.width as u32, |v| v.get()),
            configure
                .new_size
                .1
                .map_or(self.app.height as u32, |v| v.get()),
        );
        self.app.maximized = configure.state.contains(WindowState::MAXIMIZED)
            || configure.state.contains(WindowState::FULLSCREEN);
        let focused = configure.state.contains(WindowState::ACTIVATED);
        if focused != self.app.focused {
            self.app.focused = focused;
            self.app.dirty = true;
        }
        self.app.set_size(w, h);
        if !self.configured {
            self.configured = true;
            self.ensure_mode_geometry();
            self.app.dirty = true;
        }
    }
}

impl SeatHandler for CalcWin {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if self.data_device.is_none() {
            if let Some(manager) = &self.data_devices {
                self.data_device = Some(manager.get_data_device(qh, &seat));
            }
        }
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
                .get_pointer_with_theme(
                    qh,
                    &seat,
                    self.shm.wl_shm(),
                    surface,
                    ThemeSpec::default(),
                )
                .ok();
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
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

impl KeyboardHandler for CalcWin {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        serial: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        self.serial = serial;
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        self.ctrl = false;
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        self.serial = serial;
        self.key(event);
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        m: Modifiers,
        _: u32,
    ) {
        self.ctrl = m.ctrl || m.logo;
    }
}

impl PointerHandler for CalcWin {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.window.wl_surface() {
                continue;
            }
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            let action = match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.app.handle_pointer_motion(x, y)
                }
                PointerEventKind::Leave { .. } => self.app.handle_pointer_motion(-1.0, -1.0),
                PointerEventKind::Press { button, serial, .. } => {
                    self.serial = serial;
                    if button == 0x110 {
                        self.app.handle_click(x, y)
                    } else {
                        Action::None
                    }
                }
                PointerEventKind::Release { .. } | PointerEventKind::Axis { .. } => Action::None,
            };
            self.apply_action(action);
        }
    }
}

impl ShmHandler for CalcWin {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl DataDeviceHandler for CalcWin {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
        _: &wl_surface::WlSurface,
    ) {
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for CalcWin {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl DataSourceHandler for CalcWin {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        _: String,
        mut fd: WritePipe,
    ) {
        if let Some((_, text)) = self
            .copy_source
            .as_ref()
            .filter(|(s, _)| s.inner() == source)
        {
            let bytes = text.clone().into_bytes();
            std::thread::spawn(move || {
                use std::io::Write;
                let _ = fd.write_all(&bytes);
            });
        }
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self
            .copy_source
            .as_ref()
            .is_some_and(|(s, _)| s.inner() == source)
        {
            self.copy_source = None;
        }
    }
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

delegate_compositor!(CalcWin);
delegate_output!(CalcWin);
delegate_shm!(CalcWin);
delegate_seat!(CalcWin);
delegate_keyboard!(CalcWin);
delegate_pointer!(CalcWin);
delegate_xdg_shell!(CalcWin);
delegate_xdg_window!(CalcWin);
delegate_data_device!(CalcWin);
delegate_registry!(CalcWin);

impl ProvidesRegistryState for CalcWin {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpFractionalScaleV1, ()> for CalcWin {
    fn event(
        win: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
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
        impl Dispatch<$proxy, ()> for CalcWin {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
no_events!(WpFractionalScaleManagerV1, WpViewporter, WpViewport);
