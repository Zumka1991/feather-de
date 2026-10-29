//! Текстовый редактор (`feather-textedit`) — native Wayland text & code editor in macOS Tahoe style.
//! Zero idle CPU: event-driven `calloop` + `wl_shm` shared-memory rendering with `tiny-skia`.

mod app;
mod theme;

use app::{App, Cursor, Effect, Mods};
use feather_ui::fileops::parse_uri_list;
use feather_ui::i18n::tr;
use feather_ui::text;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Instant;

const URI_LIST: &str = "text/uri-list";

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
    Loaded {
        path: PathBuf,
        content: Result<String, String>,
    },
    Saved {
        doc_id: u64,
        path: PathBuf,
        content: String,
        result: Result<(), String>,
    },
    SaveDialogPicked {
        doc_id: u64,
        path: PathBuf,
    },
    Paste(String),
    Dropped(Vec<PathBuf>),
    PickerClosed,
}

struct TextEdit {
    _appearance_watch: Option<feather_ui::appearance::Watcher>,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    data_devices: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    copy_source: Option<(CopyPasteSource, String)>,
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
    cursor: Option<Cursor>,
    serial: u32,
    mods: Mods,
    tx: channel::Sender<Msg>,
    handle: LoopHandle<'static, TextEdit>,
    picker_open: bool,
    conn: Connection,
    qh: QueueHandle<TextEdit>,
    pointer_pos: (f32, f32),
}

fn spawn_load(path: PathBuf, tx: channel::Sender<Msg>) {
    let _ = std::thread::Builder::new()
        .name("feather-textedit-load".into())
        .spawn(move || {
            let content = match std::fs::read(&path) {
                Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
                Err(e) => Err(format!("{}: {e}", tr("Не удалось открыть файл"))),
            };
            let _ = tx.send(Msg::Loaded { path, content });
        });
}

fn spawn_write(doc_id: u64, path: PathBuf, content: String, tx: channel::Sender<Msg>) {
    let _ = std::thread::Builder::new()
        .name("feather-textedit-save".into())
        .spawn(move || {
            let res = std::fs::write(&path, content.as_bytes())
                .map_err(|e| format!("{}: {e}", tr("Ошибка сохранения")));
            let _ = tx.send(Msg::Saved {
                doc_id,
                path,
                content,
                result: res,
            });
        });
}

fn spawn_open_dialog(start_dir: Option<Vec<u8>>, tx: channel::Sender<Msg>) {
    let _ = std::thread::Builder::new()
        .name("feather-textedit-open-dlg".into())
        .spawn(move || {
            use feather_ui::picker::{Config, Filter, Mode};
            let cfg = Config {
                title: tr("Открыть текстовый документ").into_owned(),
                accept: tr("Открыть").into_owned(),
                mode: Mode::Open,
                multiple: true,
                start: start_dir,
                name: String::new(),
                filters: vec![
                    Filter {
                        name: tr("Текстовые файлы и код").into_owned(),
                        rules: vec![
                            (0, "*.txt".into()),
                            (0, "*.md".into()),
                            (0, "*.rs".into()),
                            (0, "*.toml".into()),
                            (0, "*.json".into()),
                            (0, "*.yaml".into()),
                            (0, "*.yml".into()),
                            (0, "*.sh".into()),
                            (0, "*.py".into()),
                            (0, "*.js".into()),
                            (0, "*.ts".into()),
                            (0, "*.c".into()),
                            (0, "*.h".into()),
                            (0, "*.cpp".into()),
                            (0, "*.html".into()),
                            (0, "*.css".into()),
                            (0, "*.conf".into()),
                            (0, "*.ini".into()),
                            (0, "*.log".into()),
                        ],
                    },
                    Filter {
                        name: tr("Все файлы").into_owned(),
                        rules: vec![(0, "*".into())],
                    },
                ],
                filter: 0,
                choices: Vec::new(),
            };
            if let Ok(Some(sel)) = feather_ui::picker::run(&cfg, None, |_| {}) {
                if let Ok(paths) = sel.local_paths() {
                    for path in paths {
                        spawn_load(path, tx.clone());
                    }
                }
            }
            let _ = tx.send(Msg::PickerClosed);
        });
}

fn spawn_save_dialog(
    doc_id: u64,
    start_dir: Option<Vec<u8>>,
    suggested_name: String,
    tx: channel::Sender<Msg>,
) {
    let _ = std::thread::Builder::new()
        .name("feather-textedit-save-dlg".into())
        .spawn(move || {
            use feather_ui::picker::{Config, Filter, Mode};
            let cfg = Config {
                title: tr("Сохранить документ").into_owned(),
                accept: tr("Сохранить").into_owned(),
                mode: Mode::Save,
                multiple: false,
                start: start_dir,
                name: suggested_name,
                filters: vec![
                    Filter {
                        name: tr("Текстовые файлы и код").into_owned(),
                        rules: vec![
                            (0, "*.txt".into()),
                            (0, "*.md".into()),
                            (0, "*.rs".into()),
                            (0, "*.toml".into()),
                            (0, "*.json".into()),
                            (0, "*.sh".into()),
                            (0, "*.py".into()),
                        ],
                    },
                    Filter {
                        name: tr("Все файлы").into_owned(),
                        rules: vec![(0, "*".into())],
                    },
                ],
                filter: 0,
                choices: Vec::new(),
            };
            if let Ok(Some(sel)) = feather_ui::picker::run(&cfg, None, |_| {}) {
                if let Ok(mut paths) = sel.local_paths() {
                    if let Some(first) = paths.drain(..).next() {
                        let _ = tx.send(Msg::SaveDialogPicked {
                            doc_id,
                            path: first,
                        });
                        let _ = tx.send(Msg::PickerClosed);
                        return;
                    }
                }
            }
            let _ = tx.send(Msg::PickerClosed);
        });
}

fn parse_args() -> Vec<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    let mut paths = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--sample" {
            let path = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp/feather-sample-textedit.md"));
            let _ = std::fs::write(&path, app::sample_markdown_content().as_bytes());
            paths.push(path);
        } else if !arg.to_string_lossy().starts_with('-') {
            paths.push(PathBuf::from(arg));
        }
    }
    paths
}

fn main() {
    // Started from an older Files or a script with SIGCHLD ignored: our own waits need it back
    feather_ui::process::default_child_signal();
    feather_ui::appearance::init();
    let started = Instant::now();
    let initial_paths = parse_args();

    let conn = Connection::connect_to_env().expect("Нет Wayland-композитора (WAYLAND_DISPLAY)");
    let (globals, queue) = registry_queue_init(&conn).expect("Реестр Wayland недоступен");
    let qh = queue.handle();
    let mut event_loop: EventLoop<TextEdit> = EventLoop::try_new().expect("цикл событий");
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue)
        .insert(handle.clone())
        .expect("источник Wayland");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let xdg = XdgShell::bind(&globals, &qh).expect("xdg_wm_base");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let surface = compositor.create_surface(&qh);
    let window = xdg.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_title(tr("Текстовый редактор"));
    window.set_app_id("feather-textedit");
    window.set_min_size(Some((580, 380)));

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
        .insert_source(rx, |event, _, editor| {
            if let channel::Event::Msg(msg) = event {
                match msg {
                    Msg::Appearance(appearance) => {
                        if feather_ui::appearance::sync(appearance) {
                            editor.app.invalidate_layout();
                        }
                    }
                    Msg::Loaded { path, content } => match content {
                        Ok(text) => {
                            let effects = editor.app.open_loaded_file(path, text);
                            editor.run_effects(effects);
                        }
                        Err(e) => editor.app.set_error(e),
                    },
                    Msg::Saved {
                        doc_id,
                        path,
                        content,
                        result,
                    } => match result {
                        Ok(()) => {
                            let effects = editor.app.mark_saved(doc_id, path, content);
                            editor.run_effects(effects);
                        }
                        Err(e) => editor.app.set_error(e),
                    },
                    Msg::SaveDialogPicked { doc_id, path } => {
                        let content = editor.app.doc().text.clone();
                        spawn_write(doc_id, path, content, editor.tx.clone());
                    }
                    Msg::Paste(text) => {
                        let effects = editor.app.paste_text(&text);
                        editor.run_effects(effects);
                    }
                    Msg::Dropped(paths) => {
                        for path in paths {
                            if path.is_file() {
                                spawn_load(path, editor.tx.clone());
                            }
                        }
                    }
                    Msg::PickerClosed => {
                        editor.picker_open = false;
                    }
                }
            }
        })
        .expect("канал результатов");

    for path in initial_paths {
        spawn_load(path, tx.clone());
    }

    let rgba = shm.formats().contains(&wl_shm::Format::Abgr8888);
    let mut editor = TextEdit {
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
        drop_offer: None,
        pool: SlotPool::new(960 * 640 * 4, &shm).expect("пул wl_shm"),
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
        picker_open: false,
        conn,
        qh: qh.clone(),
        pointer_pos: (0.0, 0.0),
    };

    let profile = std::env::var_os("FEATHER_PROFILE").is_some();
    let mut first_frame = true;
    while !editor.exit {
        if event_loop.dispatch(None, &mut editor).is_err() {
            break;
        }
        if editor.app.dirty {
            let t0 = Instant::now();
            if editor.draw(&qh) && profile {
                if first_frame {
                    first_frame = false;
                    eprintln!(
                        "[feather-textedit] первый кадр через {:.1} мс после запуска",
                        started.elapsed().as_secs_f64() * 1e3
                    );
                }
                eprintln!(
                    "[feather-textedit] кадр {:.2} мс",
                    t0.elapsed().as_secs_f64() * 1e3
                );
            }
        }
    }
}

impl TextEdit {
    fn current_dir_bytes(&self) -> Option<Vec<u8>> {
        self.app
            .doc()
            .path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.as_os_str().as_bytes().to_vec()))
    }

    fn run_effects(&mut self, effects: Vec<Effect>) {
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
                Effect::OpenDialog => {
                    if !self.picker_open {
                        self.picker_open = true;
                        spawn_open_dialog(self.current_dir_bytes(), self.tx.clone());
                    }
                }
                Effect::SaveDialog {
                    doc_id,
                    suggested_name,
                } => {
                    if !self.picker_open {
                        self.picker_open = true;
                        spawn_save_dialog(
                            doc_id,
                            self.current_dir_bytes(),
                            suggested_name,
                            self.tx.clone(),
                        );
                    }
                }
                Effect::WriteFile {
                    doc_id,
                    path,
                    content,
                } => {
                    spawn_write(doc_id, path, content, self.tx.clone());
                }
                Effect::Copy(text) => {
                    if let (Some(mgr), Some(dev)) = (&self.data_devices, &self.data_device) {
                        let src = mgr.create_copy_paste_source(&self.qh, TEXT_TYPES);
                        src.set_selection(dev, self.serial);
                        self.copy_source = Some((src, text));
                    }
                }
                Effect::Paste => self.paste_from_selection(),
                Effect::SetTitle(title) => {
                    self.window.set_title(title);
                }
            }
        }
    }

    fn paste_from_selection(&mut self) {
        if let Some((_, text)) = &self.copy_source {
            let text = text.clone();
            let effects = self.app.paste_text(&text);
            self.run_effects(effects);
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
            if reader.by_ref().take(1 << 20).read_to_end(&mut bytes).is_ok() {
                let _ = tx.send(Msg::Paste(String::from_utf8_lossy(&bytes).into_owned()));
            }
        });
    }

    fn update_cursor(&mut self) {
        let (x, y) = self.pointer_pos;
        let wanted = self.app.cursor_at(x, y);
        if self.cursor == Some(wanted) {
            return;
        }
        self.cursor = Some(wanted);
        let icon = match wanted {
            Cursor::Default => CursorIcon::Default,
            Cursor::Text => CursorIcon::Text,
            Cursor::Pointer => CursorIcon::Pointer,
            Cursor::Resize(e) => match e {
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

    fn key(&mut self, event: KeyEvent) {
        let effects = self
            .app
            .key(event.keysym.raw(), event.utf8.as_deref(), self.mods);
        self.run_effects(effects);
    }
}

impl CompositorHandler for TextEdit {
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

impl OutputHandler for TextEdit {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for TextEdit {
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

impl SeatHandler for TextEdit {
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
                Box::new(|editor, _, event| editor.key(event)),
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

impl KeyboardHandler for TextEdit {
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
        self.mods = Mods::default();
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
        self.mods = Mods {
            ctrl: m.ctrl || m.logo,
            shift: m.shift,
            alt: m.alt,
        };
    }
}

impl PointerHandler for TextEdit {
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
                        vertical.discrete as f32 * 44.0
                    } else {
                        vertical.absolute as f32 * 2.5
                    };
                    if dy != 0.0 {
                        self.app.scroll(dy, self.mods);
                    }
                    Vec::new()
                }
            };
            self.run_effects(effects);
        }
        self.update_cursor();
    }
}

impl ShmHandler for TextEdit {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl DataDeviceHandler for TextEdit {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
        surface: &wl_surface::WlSurface,
    ) {
        let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().drag_offer()) else {
            return;
        };
        if surface != self.window.wl_surface()
            || !offer.with_mime_types(|t| t.iter().any(|m| m == URI_LIST))
        {
            offer.accept_mime_type(offer.serial, None);
            return;
        }
        offer.accept_mime_type(offer.serial, Some(URI_LIST.into()));
        offer.set_actions(DndAction::Copy, DndAction::Copy);
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {
    }

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
            let mut text = String::new();
            let mut reader = std::fs::File::from(std::os::fd::OwnedFd::from(pipe));
            let _ = reader.by_ref().take(1 << 20).read_to_string(&mut text);
            let _ = tx.send(Msg::Dropped(parse_uri_list(&text)));
        });
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for TextEdit {
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

impl DataSourceHandler for TextEdit {
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

delegate_compositor!(TextEdit);
delegate_output!(TextEdit);
delegate_shm!(TextEdit);
delegate_seat!(TextEdit);
delegate_keyboard!(TextEdit);
delegate_pointer!(TextEdit);
delegate_xdg_shell!(TextEdit);
delegate_xdg_window!(TextEdit);
delegate_data_device!(TextEdit);
delegate_registry!(TextEdit);

impl ProvidesRegistryState for TextEdit {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpFractionalScaleV1, ()> for TextEdit {
    fn event(
        editor: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let scale = scale as f64 / 120.0;
            if editor.fractional != Some(scale) {
                editor.fractional = Some(scale);
                editor.app.dirty = true;
            }
        }
    }
}

macro_rules! no_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for TextEdit {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
no_events!(WpFractionalScaleManagerV1, WpViewporter, WpViewport);
