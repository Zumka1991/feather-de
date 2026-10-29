//! Просмотр — Feather's native image & PDF viewer (`feather-preview`).
//! Drawn with tiny-skia into shared memory; all file/PDF decoding and file-picker
//! invocations run on background worker threads so the Wayland event loop never blocks.

mod app;
mod doc;
mod theme;

use app::{App, Effect, Mods};
use feather_ui::text;
use std::path::PathBuf;
use std::time::Instant;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    data_device_manager::{
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::DataSourceHandler,
        DataDeviceManagerState, WritePipe,
    },
    delegate_compositor, delegate_data_device, delegate_keyboard, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{channel, EventLoop, LoopHandle},
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
    protocol::{
        wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
        wl_data_source::WlDataSource, wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm,
        wl_surface,
    },
    Connection, Dispatch, Proxy, QueueHandle,
};

use smithay_client_toolkit::reexports::protocols::wp::{
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::{self, WpFractionalScaleV1},
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};

const URI_LIST: &str = "text/uri-list";

enum Msg {
    Appearance(feather_ui::scale::Appearance),
    Loaded(u64, Result<doc::Document, String>),
    Picked(PathBuf),
    Dropped(Vec<PathBuf>),
    PickerClosed,
}

struct Preview {
    _appearance_watch: Option<feather_ui::appearance::Watcher>,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    data_devices: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    drop_offer: Option<DragOffer>,
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
    handle: LoopHandle<'static, Preview>,
    conn: Connection,
    load_gen: u64,
    picker_open: bool,
    pointer_pos: (f32, f32),
}

fn parse_initial_path() -> Option<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--sample-pdf" {
            if let Some(target) = args.next() {
                let path = PathBuf::from(target);
                let bytes = doc::sample_pdf_bytes(&[
                    ("Feather DE — Preview", "Page 1: Native macOS Tahoe style image & PDF viewer"),
                    ("Performance Budgets", "Page 2: 0% idle CPU, <= 1 ms frame time, <= 60 MB RAM"),
                    ("Vector & Raster Graphics", "Page 3: PNG, JPEG, WebP, SVG and multi-page PDF"),
                ]);
                let _ = std::fs::write(&path, bytes);
                return Some(path);
            }
        } else if !arg.to_string_lossy().starts_with('-') {
            return Some(PathBuf::from(arg));
        }
    }
    None
}

fn main() {
    // Started from an older Files or a script with SIGCHLD ignored: our own waits need it back
    feather_ui::process::default_child_signal();
    feather_ui::appearance::init();
    let started = Instant::now();
    let initial_path = parse_initial_path();

    let conn = Connection::connect_to_env().expect("Нет Wayland-композитора (WAYLAND_DISPLAY)");
    let (globals, queue) = registry_queue_init(&conn).expect("Реестр Wayland недоступен");
    let qh = queue.handle();
    let mut event_loop: EventLoop<Preview> = EventLoop::try_new().expect("цикл событий");
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue)
        .insert(handle.clone())
        .expect("источник Wayland");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let xdg = XdgShell::bind(&globals, &qh).expect("xdg_wm_base");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let surface = compositor.create_surface(&qh);
    let window = xdg.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_title("Просмотр");
    window.set_app_id("feather-preview");
    window.set_min_size(Some((520, 360)));
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
        .insert_source(rx, |event, _, preview| {
            if let channel::Event::Msg(msg) = event {
                match msg {
                    Msg::Appearance(appearance) => {
                        if feather_ui::appearance::sync(appearance) {
                            preview.app.invalidate_layout();
                        }
                    }
                    Msg::Loaded(gen, res) => {
                        if gen == preview.load_gen {
                            if let Ok(doc) = &res {
                                preview.window.set_title(format!("{} — Просмотр", doc.title));
                            } else {
                                preview.window.set_title("Просмотр");
                            }
                            preview.app.set_document(res);
                        }
                    }
                    Msg::Picked(path) => {
                        preview.picker_open = false;
                        preview.start_load(path);
                    }
                    Msg::PickerClosed => {
                        preview.picker_open = false;
                    }
                    Msg::Dropped(paths) => {
                        let effects = preview.app.dropped(paths);
                        preview.apply(effects);
                    }
                }
            }
        })
        .expect("канал результатов");

    let rgba = shm.formats().contains(&wl_shm::Format::Abgr8888);
    let mut preview = Preview {
        _appearance_watch: {
            let tx = tx.clone();
            feather_ui::appearance::watch(move |a| tx.send(Msg::Appearance(a)).is_ok()).ok()
        },
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        data_devices: DataDeviceManagerState::bind(&globals, &qh).ok(),
        data_device: None,
        drop_offer: None,
        pool: SlotPool::new(940 * 660 * 4, &shm).expect("пул wl_shm"),
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
        conn,
        load_gen: 0,
        picker_open: false,
        pointer_pos: (0.0, 0.0),
    };

    if let Some(path) = initial_path {
        preview.start_load(path);
    }

    let profile = std::env::var_os("FEATHER_PROFILE").is_some();
    let mut first_frame = true;
    while !preview.exit {
        if event_loop.dispatch(None, &mut preview).is_err() {
            break;
        }
        if preview.app.dirty {
            let t = Instant::now();
            if preview.draw(&qh) && profile {
                if first_frame {
                    eprintln!(
                        "[feather-preview] первый кадр через {:.1} мс после запуска",
                        started.elapsed().as_secs_f64() * 1e3
                    );
                    first_frame = false;
                }
                eprintln!("[feather-preview] кадр {:.2} мс", t.elapsed().as_secs_f64() * 1e3);
            }
        }
    }
}

impl Preview {
    fn start_load(&mut self, path: PathBuf) {
        self.load_gen = self.load_gen.wrapping_add(1);
        let gen = self.load_gen;
        self.app.set_loading(&path);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let res = doc::load(&path);
            let _ = tx.send(Msg::Loaded(gen, res));
        });
    }

    fn open_file_picker(&mut self) {
        if self.picker_open {
            return;
        }
        self.picker_open = true;
        let start_dir = self
            .app
            .doc
            .as_ref()
            .and_then(|d| d.path.parent().map(|p| p.as_os_str().as_bytes().to_vec()));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            use feather_ui::picker::{Config, Filter, Mode};
            let cfg = Config {
                title: "Открыть в Просмотре".into(),
                accept: "Открыть".into(),
                mode: Mode::Open,
                multiple: false,
                start: start_dir,
                name: String::new(),
                filters: vec![
                    Filter {
                        name: "Изображения и PDF".into(),
                        rules: vec![
                            (0, "*.png".into()),
                            (0, "*.jpg".into()),
                            (0, "*.jpeg".into()),
                            (0, "*.webp".into()),
                            (0, "*.svg".into()),
                            (0, "*.pdf".into()),
                        ],
                    },
                    Filter {
                        name: "Все файлы".into(),
                        rules: vec![(0, "*".into())],
                    },
                ],
                filter: 0,
                choices: Vec::new(),
            };
            if let Ok(Some(sel)) = feather_ui::picker::run(&cfg, None, |_| {}) {
                if let Ok(mut paths) = sel.local_paths() {
                    if let Some(first) = paths.drain(..).next() {
                        let _ = tx.send(Msg::Picked(first));
                        return;
                    }
                }
            }
            let _ = tx.send(Msg::PickerClosed);
        });
    }

    fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Close => self.exit = true,
                Effect::Minimize => self.window.set_minimized(),
                Effect::ToggleMaximize => {
                    if self.app.maximized {
                        self.window.unset_maximized();
                    } else {
                        self.window.set_maximized();
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
                Effect::OpenDialog => self.open_file_picker(),
                Effect::LoadPath(path) => self.start_load(path),
            }
        }
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
            app::Cursor::Grab => CursorIcon::Grab,
            app::Cursor::Grabbing => CursorIcon::Grabbing,
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

use std::os::unix::ffi::OsStrExt;

fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let rest = line.strip_prefix("file://").unwrap_or(line);
        let path_part = if rest.starts_with('/') {
            rest
        } else if let Some(idx) = rest.find('/') {
            &rest[idx..]
        } else {
            continue;
        };
        let mut decoded = Vec::with_capacity(path_part.len());
        let bytes = path_part.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let (Some(h), Some(l)) = (
                    (bytes[i + 1] as char).to_digit(16),
                    (bytes[i + 2] as char).to_digit(16),
                ) {
                    decoded.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
            }
            decoded.push(bytes[i]);
            i += 1;
        }
        use std::os::unix::ffi::OsStringExt;
        out.push(PathBuf::from(std::ffi::OsString::from_vec(decoded)));
    }
    out
}

impl CompositorHandler for Preview {
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

impl OutputHandler for Preview {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for Preview {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, configure: WindowConfigure, _: u32) {
        let (w, h) = (
            configure.new_size.0.map_or(self.app.w, |v| v.get() as f32),
            configure.new_size.1.map_or(self.app.h, |v| v.get() as f32),
        );
        self.app.maximized = configure.state.contains(WindowState::MAXIMIZED)
            || configure.state.contains(WindowState::FULLSCREEN);
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

impl SeatHandler for Preview {
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
            let keyboard = self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.handle.clone(),
                Box::new(|preview, _, event| preview.key(event)),
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

impl KeyboardHandler for Preview {
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

impl PointerHandler for Preview {
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
                    let dy = if vertical.discrete != 0 {
                        vertical.discrete as f32 * 48.0
                    } else {
                        vertical.absolute as f32
                    };
                    self.app.scroll(dy, x, self.mods);
                    Vec::new()
                }
            };
            self.apply(effects);
        }
        self.update_cursor();
    }
}

impl ShmHandler for Preview {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl DataDeviceHandler for Preview {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64, surface: &wl_surface::WlSurface) {
        let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().drag_offer()) else {
            return;
        };
        if surface != self.window.wl_surface() || !offer.with_mime_types(|t| t.iter().any(|m| m == URI_LIST)) {
            offer.accept_mime_type(offer.serial, None);
            return;
        }
        offer.accept_mime_type(offer.serial, Some(URI_LIST.into()));
        offer.set_actions(DndAction::Copy, DndAction::Copy);
        self.app.drag_over();
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        self.app.drag_leave();
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().drag_offer()) else {
            return;
        };
        let Ok(pipe) = offer.receive(URI_LIST.into()) else {
            return;
        };
        offer.finish();
        self.drop_offer = Some(offer);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut text = String::new();
            let mut reader = std::fs::File::from(std::os::fd::OwnedFd::from(pipe));
            let _ = reader.by_ref().take(1 << 20).read_to_string(&mut text);
            let _ = tx.send(Msg::Dropped(parse_uri_list(&text)));
        });
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for Preview {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

impl DataSourceHandler for Preview {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: String, _: WritePipe) {}
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

delegate_compositor!(Preview);
delegate_output!(Preview);
delegate_shm!(Preview);
delegate_seat!(Preview);
delegate_keyboard!(Preview);
delegate_pointer!(Preview);
delegate_xdg_shell!(Preview);
delegate_xdg_window!(Preview);
delegate_data_device!(Preview);
delegate_registry!(Preview);

impl ProvidesRegistryState for Preview {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpFractionalScaleV1, ()> for Preview {
    fn event(
        preview: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let scale = scale as f64 / 120.0;
            if preview.fractional != Some(scale) {
                preview.fractional = Some(scale);
                preview.app.dirty = true;
            }
        }
    }
}

macro_rules! no_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for Preview {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
no_events!(WpFractionalScaleManagerV1, WpViewporter, WpViewport);
