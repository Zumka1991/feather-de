//! Заметки — Feather's notes: a small Wayland client drawn with tiny-skia into shared
//! memory. Idle costs nothing: only input, a one-shot save timer after typing and results
//! of short worker threads wake it up.

mod app;
mod store;
mod theme;

use feather_ui::text;
use std::time::{Duration, Instant};

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
        calloop::{
            channel,
            timer::{TimeoutAction, Timer},
            EventLoop, LoopHandle,
        },
        calloop_wayland_source::WaylandSource,
        csd_frame::{CursorIcon, WindowState},
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
    protocol::{wl_data_device::WlDataDevice, wl_data_device_manager::DndAction, wl_data_source::WlDataSource, wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, Dispatch, Proxy, QueueHandle,
};

use smithay_client_toolkit::reexports::protocols::wp::{
    fractional_scale::v1::client::{wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, wp_fractional_scale_v1::{self, WpFractionalScaleV1}},
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};

use app::{App, Effect, Mods};

/// Text types we offer, and accept in order of preference.
const TEXT_TYPES: [&str; 5] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"];
const SAVE_DELAY: Duration = Duration::from_millis(600);

enum Msg {
    Appearance(feather_ui::scale::Appearance),
    Loaded(Vec<store::Note>),
    Paste(String),
}

struct Notes {
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
    cursor: Option<app::Cursor>,
    serial: u32,
    mods: Mods,
    tx: channel::Sender<Msg>,
    handle: LoopHandle<'static, Notes>,
    save_armed: bool,
    conn: Connection,
    qh: QueueHandle<Notes>,
    pointer_pos: (f32, f32),
}

fn main() {
    feather_ui::appearance::init();
    let started = Instant::now();
    let conn = Connection::connect_to_env().expect("Нет Wayland-композитора (WAYLAND_DISPLAY)");
    let (globals, queue) = registry_queue_init(&conn).expect("Реестр Wayland недоступен");
    let qh = queue.handle();
    let mut event_loop: EventLoop<Notes> = EventLoop::try_new().expect("цикл событий");
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue).insert(handle.clone()).expect("источник Wayland");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let xdg = XdgShell::bind(&globals, &qh).expect("xdg_wm_base");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let surface = compositor.create_surface(&qh);
    let window = xdg.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_title("Заметки");
    window.set_app_id("feather-notes");
    window.set_min_size(Some((560, 360)));
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    let fractional_manager: Option<WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
    let (viewport, fractional_object) = match (&viewporter, &fractional_manager) {
        (Some(v), Some(f)) => (Some(v.get_viewport(window.wl_surface(), &qh, ())), Some(f.get_fractional_scale(window.wl_surface(), &qh, ()))),
        _ => (None, None),
    };
    window.commit();

    let (tx, rx) = channel::channel::<Msg>();
    handle
        .insert_source(rx, |event, _, notes| {
            if let channel::Event::Msg(msg) = event {
                match msg {
                    Msg::Appearance(appearance) => {
                        if std::env::var_os("FEATHER_CLIENT_UI_CHECK").is_some() { eprintln!("[feather-notes-ui-check] appearance={appearance:?}"); }
                        if feather_ui::appearance::sync(appearance) {
                            notes.app.invalidate_layout();
                        }
                    }
                    Msg::Loaded(list) => notes.app.loaded(list),
                    Msg::Paste(text) => notes.app.paste_text(&text),
                }
            }
        })
        .expect("канал результатов");
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Loaded(store::load()));
        });
    }

    let rgba = shm.formats().contains(&wl_shm::Format::Abgr8888);
    let mut notes = Notes {
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
        pool: SlotPool::new(920 * 620 * 4, &shm).expect("пул wl_shm"),
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
        app: App::new(text::FontSystem::new()),
        seat: None,
        keyboard: None,
        pointer: None,
        cursor: None,
        serial: 0,
        mods: Mods::default(),
        tx,
        handle: handle.clone(),
        save_armed: false,
        conn,
        qh: qh.clone(),
        pointer_pos: (0.0, 0.0),
    };
    let profile = std::env::var_os("FEATHER_PROFILE").is_some();
    let mut first_frame = true;
    while !notes.exit {
        if event_loop.dispatch(None, &mut notes).is_err() {
            break;
        }
        notes.arm_save_timer();
        if notes.app.dirty {
            let t = Instant::now();
            if notes.draw(&qh) && profile {
                if first_frame {
                    eprintln!("[feather-notes] первый кадр через {:.1} мс после запуска", started.elapsed().as_secs_f64() * 1e3);
                    first_frame = false;
                }
                eprintln!("[feather-notes] кадр {:.2} мс", t.elapsed().as_secs_f64() * 1e3);
            }
        }
    }
    // Nothing typed may be lost on close: written synchronously here.
    for effect in notes.app.on_exit() {
        match effect {
            Effect::Save(id, text) => {
                if let Err(e) = store::save(id, &text) {
                    eprintln!("Заметки: не удалось сохранить: {e}");
                }
            }
            Effect::Remove(id) => store::remove(id),
            _ => {}
        }
    }
}

impl Notes {
    fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Close => self.exit = true,
                Effect::Minimize => self.window.set_minimized(),
                Effect::ToggleMaximize => {
                    if self.app.maximized { self.window.unset_maximized() } else { self.window.set_maximized() }
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
                Effect::Save(id, text) => {
                    std::thread::spawn(move || {
                        if let Err(e) = store::save(id, &text) {
                            eprintln!("Заметки: не удалось сохранить: {e}");
                        }
                    });
                }
                Effect::Trash(id) => {
                    std::thread::spawn(move || {
                        store::trash(id);
                    });
                }
                Effect::Remove(id) => {
                    std::thread::spawn(move || store::remove(id));
                }
                Effect::Copy(text) => self.copy(text),
                Effect::Paste => self.paste(),
            }
        }
    }

    /// One save after a pause in typing; no periodic timer.
    fn arm_save_timer(&mut self) {
        if self.save_armed || !self.app.has_unsaved() {
            return;
        }
        self.save_armed = true;
        let _ = self.handle.insert_source(Timer::from_duration(SAVE_DELAY), |_, _, notes| {
            notes.save_armed = false;
            let effects: Vec<Effect> = notes.app.take_save().into_iter().collect();
            notes.apply(effects);
            TimeoutAction::Drop
        });
    }

    fn copy(&mut self, text: String) {
        let (Some(manager), Some(device)) = (self.data_devices.as_ref(), self.data_device.as_ref()) else { return };
        let qh = &self.handle_qh();
        let source = manager.create_copy_paste_source(qh, TEXT_TYPES);
        source.set_selection(device, self.serial);
        self.copy_source = Some((source, text));
    }

    fn paste(&mut self) {
        if let Some((_, text)) = &self.copy_source {
            let text = text.clone();
            self.app.paste_text(&text);
            return;
        }
        let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().selection_offer()) else { return };
        let Some(mime) = offer.with_mime_types(|types| TEXT_TYPES.iter().find(|t| types.iter().any(|m| m == *t)).map(|t| t.to_string())) else { return };
        let Ok(pipe) = offer.receive(mime) else { return };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut bytes = Vec::new();
            let mut reader = std::fs::File::from(std::os::fd::OwnedFd::from(pipe));
            if reader.by_ref().take(1 << 20).read_to_end(&mut bytes).is_ok() {
                let _ = tx.send(Msg::Paste(String::from_utf8_lossy(&bytes).into_owned()));
            }
        });
    }

    fn handle_qh(&self) -> QueueHandle<Self> {
        self.qh.clone()
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
        theme::set_ui_scale(scale);
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
        let effects = self.app.key(event.keysym.raw(), event.utf8.as_deref(), self.mods);
        self.apply(effects);
    }
}

impl CompositorHandler for Notes {
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

impl OutputHandler for Notes {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for Notes {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
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
        }
        self.app.resize(w, h);
        if !self.configured {
            self.configured = true;
            self.app.dirty = true;
        }
    }
}

impl SeatHandler for Notes {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        if self.data_device.is_none() {
            if let Some(manager) = &self.data_devices {
                self.data_device = Some(manager.get_data_device(qh, &seat));
            }
        }
        self.seat = Some(seat.clone());
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let keyboard = self.seat_state.get_keyboard_with_repeat(qh, &seat, None, self.handle.clone(), Box::new(|notes, _, event| notes.key(event)));
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

impl KeyboardHandler for Notes {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, serial: u32, _: &[u32], _: &[Keysym]) {
        self.serial = serial;
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, serial: u32, event: KeyEvent) {
        self.serial = serial;
        self.key(event);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, m: Modifiers, _: u32) {
        self.mods = Mods { ctrl: m.ctrl, shift: m.shift };
    }
}

impl PointerHandler for Notes {
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
                    self.app.press(button, x, y, self.mods)
                }
                PointerEventKind::Release { button, serial, .. } => {
                    self.serial = serial;
                    self.app.release(button);
                    Vec::new()
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let dy = if vertical.discrete != 0 { vertical.discrete as f32 * 48.0 } else { vertical.absolute as f32 };
                    self.app.scroll(dy, x);
                    Vec::new()
                }
            };
            self.apply(effects);
        }
        self.update_cursor();
    }
}

impl ShmHandler for Notes {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl DataDeviceHandler for Notes {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64, _: &wl_surface::WlSurface) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    /// Another program copied: our own text is no longer the clipboard.
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for Notes {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

impl DataSourceHandler for Notes {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource, _: String, pipe: WritePipe) {
        let Some((_, text)) = self.copy_source.as_ref().filter(|(s, _)| s.inner() == source) else { return };
        let data = text.clone();
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = std::fs::File::from(std::os::fd::OwnedFd::from(pipe)).write_all(data.as_bytes());
        });
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self.copy_source.as_ref().is_some_and(|(s, _)| s.inner() == source) {
            self.copy_source = None;
        }
    }
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

delegate_compositor!(Notes);
delegate_output!(Notes);
delegate_shm!(Notes);
delegate_seat!(Notes);
delegate_keyboard!(Notes);
delegate_pointer!(Notes);
delegate_xdg_shell!(Notes);
delegate_xdg_window!(Notes);
delegate_data_device!(Notes);
delegate_registry!(Notes);

impl ProvidesRegistryState for Notes {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpFractionalScaleV1, ()> for Notes {
    fn event(notes: &mut Self, _: &WpFractionalScaleV1, event: wp_fractional_scale_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let scale = scale as f64 / 120.0;
            if notes.fractional != Some(scale) {
                notes.fractional = Some(scale);
                notes.app.dirty = true;
            }
        }
    }
}

macro_rules! no_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for Notes {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
no_events!(WpFractionalScaleManagerV1, WpViewporter, WpViewport);
