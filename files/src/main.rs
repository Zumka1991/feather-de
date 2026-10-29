//! Файлы — the Feather file manager: a small Wayland client that draws itself with
//! tiny-skia straight into shared memory. Idle costs nothing: there is no timer or polling,
//! only input, inotify on the open folder and results of short worker threads wake it up.

mod app;
mod fsops;
mod theme;
mod preview;

use feather_ui::{draw as ui, file_icons as icons, text};

use std::{path::PathBuf, time::Instant};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    data_device_manager::{
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::{CopyPasteSource, DataSourceHandler, DragSource},
        DataDeviceManagerState, WritePipe,
    },
    delegate_compositor, delegate_data_device, delegate_keyboard, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            channel,
            generic::Generic,
            timer::{TimeoutAction, Timer},
            EventLoop, Interest, LoopHandle, Mode, PostAction,
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
    Connection, QueueHandle,
};

use smithay_client_toolkit::reexports::protocols::wp::{
    fractional_scale::v1::client::{wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, wp_fractional_scale_v1::{self, WpFractionalScaleV1}},
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};
use wayland_client::{Dispatch, Proxy};

use app::{App, Effect, Mods};

const URI_LIST: &str = "text/uri-list";

/// Results of worker threads
enum Msg {
    Appearance(feather_ui::scale::Appearance),
    Preview(u64, Result<preview::Content, String>),
    SaveChecked(PathBuf,Result<Option<bool>,String>),
    Listed(u64, Result<Vec<fsops::Entry>, String>),
    Done(usize, &'static str),
    /// A paste copied more (throttled), stopped on a taken name, or finished (failures)
    JobTick,
    Conflict(u64, PathBuf, bool),
    PasteDone(u64, usize),
    Props(app::Props),
    ExternalFiles(Vec<PathBuf>),
    /// Files of a drop, read from the offer; the offer is finished after the paste starts
    Dropped(Vec<PathBuf>, PathBuf, bool),
}

struct Files {
    preview_debug_armed: bool,
    _appearance_watch: Option<feather_ui::appearance::Watcher>,
    preview_tx: std::sync::mpsc::SyncSender<(u64, PathBuf)>,
    preview_pending: Option<(u64, PathBuf)>,
    preview_newest: std::sync::Arc<std::sync::atomic::AtomicU64>,
    picker_test_armed:bool,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    data_devices: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    copy_source: Option<(CopyPasteSource, Vec<PathBuf>)>,
    /// Our drag: source, its files and the icon surface under the pointer
    drag_source: Option<(DragSource, Vec<PathBuf>, wl_surface::WlSurface)>,
    /// A drop being read: finished once the files arrived
    drop_offer: Option<DragOffer>,
    /// Folder the current drag over the window would drop into
    drop_dir: Option<PathBuf>,
    window: Window,
    pool: SlotPool,
    /// tiny-skia's RGBA byte order; Argb8888 needs a channel swap
    rgba: bool,
    scale: i32,
    /// wp_fractional_scale_v1 preferred scale; with a viewport the buffer is drawn at it exactly
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
    /// Answers for paste workers waiting on «уже существует», by job id
    answers: std::collections::HashMap<u64, std::sync::mpsc::Sender<(fsops::Conflict, bool)>>,
    handle: LoopHandle<'static, Files>,
    watcher: Option<fsops::Watcher>,
    watched: Option<PathBuf>,
    reload_armed: bool,
    status_armed: bool,
    conn: Connection,
    qh: QueueHandle<Files>,
    pointer_pos: (f32, f32),
}

fn main() {
    feather_ui::appearance::init();
    // Children (xdg-open, terminals) are reaped by the kernel; we never wait for them
    // Programs we open must not inherit an ignored SIGCHLD (see feather_ui::process)
    feather_ui::process::reap_children();
    let started = Instant::now();
    // feather-files [--rename] [path]: a file path opens its folder with the file selected
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let picker_config=args.iter().any(|a|a.as_os_str()=="--file-picker").then(||feather_ui::picker::read_config().expect("запрос выбора файла"));
    let rename = args.iter().any(|a| a.as_os_str() == "--rename");
    let target = if let Some(config)=&picker_config{use std::os::unix::ffi::OsStringExt;config.start.as_ref().map(|p|PathBuf::from(std::ffi::OsString::from_vec(p.clone())))}else{args.into_iter().find(|a| a.as_os_str() != "--rename"&&a.as_os_str()!="--file-picker")};
    let (start, reveal) = match target {
        Some(p) if p.is_dir() && !rename => (p, None),
        Some(p) => (p.parent().map(PathBuf::from).unwrap_or_else(fsops::home), Some(p)),
        None => (fsops::home(), None),
    };

    let conn = Connection::connect_to_env().expect("Нет Wayland-композитора (WAYLAND_DISPLAY)");
    let (globals, queue) = registry_queue_init(&conn).expect("Реестр Wayland недоступен");
    let qh = queue.handle();
    let mut event_loop: EventLoop<Files> = EventLoop::try_new().expect("цикл событий");
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue).insert(handle.clone()).expect("источник Wayland");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let xdg = XdgShell::bind(&globals, &qh).expect("xdg_wm_base");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let surface = compositor.create_surface(&qh);
    let window = xdg.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_title("Файлы");
    window.set_app_id("feather-files");
    window.set_min_size(Some((480, 320)));
    // Exact (also fractional) HiDPI: the compositor tells the scale, the viewport maps the
    // larger buffer onto the logical window size
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    let fractional_manager: Option<WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
    let (viewport, fractional_object) = match (&viewporter, &fractional_manager) {
        (Some(v), Some(f)) => (Some(v.get_viewport(window.wl_surface(), &qh, ())), Some(f.get_fractional_scale(window.wl_surface(), &qh, ()))),
        _ => (None, None),
    };
    window.commit();

    let (tx, rx) = channel::channel::<Msg>();
    handle
        .insert_source(rx, |event, _, files| {
            if let channel::Event::Msg(msg) = event {
                let effects = match msg {
                    Msg::Appearance(appearance) => {
                        if std::env::var_os("FEATHER_CLIENT_UI_CHECK").is_some() { eprintln!("[feather-files-ui-check] appearance={appearance:?}"); }
                        if feather_ui::appearance::sync(appearance) {
                            files.app.dirty = true;
                        }
                        Vec::new()
                    }
                    Msg::Preview(generation, result) => {
                        if std::env::var_os("FEATHER_CLIENT_UI_CHECK").is_some() { eprintln!("[feather-files-ui-check] preview-ready generation={generation} ok={}", result.is_ok()); }
                        files.app.preview_ready(generation, result);
                        files.flush_preview();
                        Vec::new()
                    }
                    Msg::SaveChecked(path,result)=>files.app.save_checked(path,result),
                    Msg::Listed(generation, result) => {
                        files.app.listed(generation, result);
                        if !files.picker_test_armed{
                            if let Some(path)=std::env::var_os("FEATHER_PICKER_TEST_FILE"){
                                files.picker_test_armed=true;let path=PathBuf::from(path);
                                let delay=std::env::var("FEATHER_PICKER_TEST_DELAY_MS").ok().and_then(|s|s.parse::<u64>().ok()).unwrap_or(600);
                                let _=files.handle.insert_source(Timer::from_duration(std::time::Duration::from_millis(delay)),move|_,_,files|{let effects=files.app.debug_picker_choose(&path);files.apply(effects);TimeoutAction::Drop});
                            }
                        }
                        if !files.preview_debug_armed {
                            if let Some(path) = std::env::var_os("FEATHER_FILES_DEBUG_PREVIEW") {
                                files.preview_debug_armed = true;
                                files.app.debug_preview(&PathBuf::from(path))
                            } else { Vec::new() }
                        } else { Vec::new() }
                    }
                    Msg::Done(failed, what) => files.app.operation_done(failed, what),
                    Msg::JobTick => {
                        files.app.job_tick();
                        Vec::new()
                    }
                    Msg::Conflict(id, target, is_dir) => {
                        files.app.job_conflict(id, target, is_dir);
                        Vec::new()
                    }
                    Msg::PasteDone(id, failed) => {
                        files.answers.remove(&id);
                        files.app.paste_done(id, failed)
                    }
                    Msg::Props(props) => {
                        files.app.props_ready(props);
                        Vec::new()
                    }
                    Msg::Dropped(paths, dir, move_allowed) => {
                        let effects = files.app.drop_files(paths, dir, move_allowed);
                        if let Some(offer) = files.drop_offer.take() {
                            offer.finish();
                            offer.destroy();
                        }
                        effects
                    }
                    Msg::ExternalFiles(paths) => {
                        files.app.external_clipboard(paths);
                        Vec::new()
                    }
                };
                files.apply(effects);
            }
        })
        .expect("канал результатов");

    let fonts = text::FontSystem::new();
    let (preview_tx, preview_rx) = std::sync::mpsc::sync_channel::<(u64, PathBuf)>(1);
    let preview_newest = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    {
        let tx = tx.clone();
        let newest = preview_newest.clone();
        std::thread::Builder::new().name("feather-preview".into()).stack_size(256 * 1024).spawn(move || {
            while let Ok((generation, path)) = preview_rx.recv() {
                let result = if newest.load(std::sync::atomic::Ordering::Relaxed) == generation {
                    preview::load(&path)
                } else { Err(String::new()) };
                if tx.send(Msg::Preview(generation, result)).is_err() { break; }
            }
        }).expect("поток предпросмотра");
    }
    let (mut app, effects) = App::new(fonts, start, reveal, rename);
    if let Some(config)=picker_config{app.set_picker(config);window.set_title(app.window_title());window.set_min_size(Some((640,(app.picker_height() as u32+160).max(320))));}
    let watcher = fsops::Watcher::new();
    if let Some(w) = &watcher {
        let fd = w.fd();
        // SAFETY: the fd belongs to the watcher, which lives as long as the event loop state
        let source = Generic::new(unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }, Interest::READ, Mode::Level);
        handle
            .insert_source(source, |_, _, files| {
                if files.watcher.as_ref().is_some_and(fsops::Watcher::drain) {
                    files.schedule_reload();
                }
                Ok(PostAction::Continue)
            })
            .expect("inotify");
    }
    let rgba = shm.formats().contains(&wl_shm::Format::Abgr8888);
    let mut files = Files {
        preview_debug_armed: false,
        _appearance_watch: {
            let tx = tx.clone();
            feather_ui::appearance::watch(move |a| tx.send(Msg::Appearance(a)).is_ok()).ok()
        },
        preview_tx,
        preview_pending: None,
        preview_newest,
        picker_test_armed:false,
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        data_devices: DataDeviceManagerState::bind(&globals, &qh).ok(),
        data_device: None,
        copy_source: None,
        drag_source: None,
        drop_offer: None,
        drop_dir: None,
        pool: SlotPool::new(1000 * 640 * 4, &shm).expect("пул wl_shm"),
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
        cursor: None,
        serial: 0,
        mods: Mods::default(),
        tx,
        answers: Default::default(),
        handle: handle.clone(),
        watcher,
        watched: None,
        reload_armed: false,
        status_armed: false,
        conn,
        qh: qh.clone(),
        pointer_pos: (0.0, 0.0),
    };
    files.apply(effects);
    let profile = std::env::var_os("FEATHER_PROFILE").is_some();
    let mut first_frame = true;
    while !files.exit {
        if event_loop.dispatch(None, &mut files).is_err() {
            break;
        }
        files.arm_status_timer();
        if files.app.dirty {
            let t = Instant::now();
            if files.draw(&qh) && profile {
                if first_frame {
                    eprintln!("[feather-files] первый кадр через {:.1} мс после запуска", started.elapsed().as_secs_f64() * 1e3);
                    first_frame = false;
                }
                eprintln!("[feather-files] кадр {:.2} мс, ×{}", t.elapsed().as_secs_f64() * 1e3, files.fractional.unwrap_or(files.scale as f64));
            }
        }
    }
}

impl Files {
    fn flush_preview(&mut self) {
        if let Some(request) = self.preview_pending.take() {
            if let Err(std::sync::mpsc::TrySendError::Full(request)) = self.preview_tx.try_send(request) {
                self.preview_pending = Some(request);
            }
        }
    }

    fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Preview(path, generation) => {
                    self.preview_newest.store(generation, std::sync::atomic::Ordering::Relaxed);
                    self.preview_pending = Some((generation, path));
                    self.flush_preview();
                }
                Effect::Picked(selection)=>{if let Err(error)=feather_ui::picker::write_selection(&selection){eprintln!("{error}");}self.exit=true;},
                Effect::CheckSave(path)=>{let tx=self.tx.clone();std::thread::spawn(move||{let result=match std::fs::metadata(&path){Ok(meta)=>Ok(Some(meta.is_dir())),Err(e)if e.kind()==std::io::ErrorKind::NotFound=>Ok(None),Err(e)=>Err(e.to_string())};let _=tx.send(Msg::SaveChecked(path,result));});},
                Effect::List(dir, generation) => {
                    if self.watched.as_ref() != Some(&dir) {
                        if let Some(w) = self.watcher.as_mut() {
                            w.watch(&dir);
                        }
                        self.watched = Some(dir.clone());
                        self.window.set_title(self.app.window_title());
                    }
                    let tx = self.tx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(Msg::Listed(generation, fsops::list(&dir).map_err(|e| e.to_string())));
                    });
                }
                Effect::Close if self.app.has_jobs() => {
                    let effects = self.app.request_close();
                    self.apply(effects);
                }
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
                Effect::Paste(id, paths, dir, cut) => {
                    let Some(progress) = self.app.job_progress(id) else { continue };
                    let (answer_tx, answers) = std::sync::mpsc::channel();
                    self.answers.insert(id, answer_tx);
                    let tx = self.tx.clone();
                    std::thread::Builder::new()
                        .name("feather-paste".into())
                        .spawn(move || {
                            let mut for_all = None;
                            let mut last_tick = Instant::now();
                            let ticks = tx.clone();
                            let failed = fsops::paste_with(
                                &paths,
                                &dir,
                                cut,
                                &progress,
                                &mut |target, is_dir| {
                                    if let Some(choice) = for_all {
                                        return choice;
                                    }
                                    if tx.send(Msg::Conflict(id, target.to_path_buf(), is_dir)).is_err() {
                                        return fsops::Conflict::Skip;
                                    }
                                    let (choice, all) = answers.recv().unwrap_or((fsops::Conflict::Skip, true));
                                    if all {
                                        for_all = Some(choice);
                                    }
                                    choice
                                },
                                &mut || {
                                    if last_tick.elapsed() >= std::time::Duration::from_millis(100) {
                                        last_tick = Instant::now();
                                        let _ = ticks.send(Msg::JobTick);
                                    }
                                },
                            );
                            let _ = tx.send(Msg::PasteDone(id, failed));
                        })
                        .expect("поток копирования");
                }
                Effect::Resolve(id, choice, all) => {
                    if let Some(answer) = self.answers.get(&id) {
                        let _ = answer.send((choice, all));
                    }
                }
                Effect::Trash(paths) => {
                    let tx = self.tx.clone();
                    std::thread::spawn(move || {
                        let failed = fsops::trash(&paths);
                        let _ = tx.send(Msg::Done(failed, "Перемещение в Корзину"));
                    });
                }
                Effect::Restore(paths) => {
                    let tx = self.tx.clone();
                    std::thread::spawn(move || {
                        let failed = fsops::restore(&paths);
                        let _ = tx.send(Msg::Done(failed, "Возврат из Корзины"));
                    });
                }
                Effect::DeleteForever(paths) => {
                    let tx = self.tx.clone();
                    std::thread::spawn(move || {
                        let failed = fsops::delete_forever(&paths);
                        let _ = tx.send(Msg::Done(failed, "Удаление"));
                    });
                }
                Effect::EmptyTrash => {
                    let tx = self.tx.clone();
                    std::thread::spawn(move || {
                        let failed = fsops::empty_trash();
                        let _ = tx.send(Msg::Done(failed, "Очистка Корзины"));
                    });
                }
                Effect::Inspect(path) => {
                    let tx = self.tx.clone();
                    std::thread::spawn(move || inspect(path, &tx));
                }
                Effect::Clipboard(paths) => self.offer_files(paths),
                Effect::StartDrag(paths) => self.start_drag(paths),
            }
        }
    }

    /// Other programs (file managers, chats) paste our files as text/uri-list
    fn offer_files(&mut self, paths: Vec<PathBuf>) {
        let (Some(manager), Some(device)) = (self.data_devices.as_ref(), self.data_device.as_ref()) else { return };
        let source = manager.create_copy_paste_source(&self.qh, [URI_LIST, "text/plain;charset=utf-8", "text/plain"]);
        source.set_selection(device, self.serial);
        self.copy_source = Some((source, paths));
    }

    /// Wayland DnD with the files; the compositor draws the icon surface at the pointer
    fn start_drag(&mut self, paths: Vec<PathBuf>) {
        let (Some(manager), Some(device)) = (self.data_devices.as_ref(), self.data_device.as_ref()) else { return };
        use wayland_client::protocol::wl_data_device_manager::DndAction;
        let source = manager.create_drag_and_drop_source(&self.qh, [URI_LIST, "text/plain;charset=utf-8", "text/plain"], DndAction::Copy | DndAction::Move);
        let icon = self.compositor.create_surface(&self.qh);
        let scale = self.fractional.unwrap_or(self.scale as f64).ceil().max(1.0) as i32;
        if let Some((px, pixels)) = self.app.drag_image(&paths, scale as f32) {
            let format = if self.rgba { wl_shm::Format::Abgr8888 } else { wl_shm::Format::Argb8888 };
            if let Ok((buffer, canvas)) = self.pool.create_buffer(px as i32, px as i32, px as i32 * 4, format) {
                ui::copy_shm_pixels(canvas, &pixels, self.rgba);
                icon.set_buffer_scale(scale);
                let _ = buffer.attach_to(&icon);
                icon.damage_buffer(0, 0, px as i32, px as i32);
                icon.commit();
            }
        }
        source.start_drag(device, self.window.wl_surface(), Some(&icon), self.serial);
        self.drag_source = Some((source, paths, icon));
    }

    /// Several inotify events in a burst (unpacking, copying) cause one re-read
    fn schedule_reload(&mut self) {
        if self.reload_armed {
            return;
        }
        self.reload_armed = true;
        let _ = self.handle.insert_source(Timer::from_duration(std::time::Duration::from_millis(120)), |_, _, files| {
            files.reload_armed = false;
            let effects = files.app.reload();
            files.apply(effects);
            TimeoutAction::Drop
        });
    }

    /// One timer only while a status message is shown
    fn arm_status_timer(&mut self) {
        if self.status_armed {
            return;
        }
        let Some(deadline) = self.app.status_deadline() else { return };
        self.status_armed = true;
        let _ = self.handle.insert_source(Timer::from_deadline(deadline), |_, _, files| {
            files.status_armed = false;
            files.app.tick();
            TimeoutAction::Drop
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
}

/// «Свойства»: stat first (shown at once), then the size of a folder. The walk is bounded so
/// a huge tree cannot keep a thread busy for minutes; it then reports "более …".
fn inspect(path: PathBuf, tx: &channel::Sender<Msg>) {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::symlink_metadata(&path) else { return };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (kind, _) = fsops::kind_of(&name, meta.is_dir(), meta.mode() & 0o111 != 0);
    let owner = {
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut buf = vec![0 as libc::c_char; 4096];
        let mut out = std::ptr::null_mut();
        let ok = unsafe { libc::getpwuid_r(meta.uid(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut out) } == 0 && !out.is_null();
        if ok { unsafe { std::ffi::CStr::from_ptr(pw.pw_name) }.to_string_lossy().into_owned() } else { meta.uid().to_string() }
    };
    let mut props = app::Props {
        path: path.clone(),
        is_dir: meta.is_dir(),
        kind: if meta.file_type().is_symlink() { "Ссылка".into() } else { kind.label().into() },
        size: (!meta.is_dir()).then_some(meta.len()),
        items: None,
        partial: false,
        modified: meta.modified().ok(),
        created: meta.created().ok(),
        accessed: meta.accessed().ok(),
        mode: meta.mode(),
        owner,
    };
    let _ = tx.send(Msg::Props(props.clone()));
    if !meta.is_dir() {
        return;
    }
    let started = Instant::now();
    let (mut size, mut items) = (0u64, 0u64);
    let mut stack = vec![path];
    while let Some(dir) = stack.pop() {
        if items > 1_000_000 || started.elapsed() > std::time::Duration::from_secs(20) {
            props.partial = true;
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let Ok(m) = e.metadata() else { continue };
            items += 1;
            size += m.len();
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    props.size = Some(size);
    props.items = Some(items);
    let _ = tx.send(Msg::Props(props));
}

impl CompositorHandler for Files {
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

impl OutputHandler for Files {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for Files {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        let effects = self.app.request_close();
        self.apply(effects);
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

impl SeatHandler for Files {
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
                Box::new(|files, _, event| files.key(event)),
            );
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

impl Files {
    fn key(&mut self, event: KeyEvent) {
        self.serial = self.serial.max(1);
        let effects = self.app.key(event.keysym.raw(), event.raw_code, event.utf8.as_deref(), self.mods);
        self.apply(effects);
    }
}

impl KeyboardHandler for Files {
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
        self.mods = Mods { ctrl: m.ctrl, shift: m.shift, alt: m.alt };
    }
}

impl PointerHandler for Files {
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
                    self.app.motion(x, y)
                }
                PointerEventKind::Leave { .. } => {
                    self.app.leave();
                    Vec::new()
                }
                PointerEventKind::Motion { .. } => self.app.motion(x, y),
                PointerEventKind::Press { button, serial, .. } => {
                    self.serial = serial;
                    self.app.press(button, x, y, self.mods)
                }
                PointerEventKind::Release { button, serial, .. } => {
                    self.serial = serial;
                    self.app.release(button, x, y)
                }
                PointerEventKind::Axis { vertical, .. } => {
                    // Wheels report discrete steps; touchpads report pixel distances
                    let dy = if vertical.discrete != 0 { vertical.discrete as f32 * 48.0 } else { vertical.absolute as f32 };
                    self.app.scroll(dy);
                    Vec::new()
                }
            };
            self.apply(effects);
        }
        self.update_cursor();
    }
}

impl ShmHandler for Files {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl DataDeviceHandler for Files {
    /// A drag entered the window: files are accepted (move preferred, as Finder does)
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, x: f64, y: f64, surface: &wl_surface::WlSurface) {
        let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().drag_offer()) else { return };
        if surface != self.window.wl_surface() || !offer.with_mime_types(|t| t.iter().any(|m| m == URI_LIST)) {
            offer.accept_mime_type(offer.serial, None);
            return;
        }
        use wayland_client::protocol::wl_data_device_manager::DndAction;
        offer.accept_mime_type(offer.serial, Some(URI_LIST.into()));
        offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Move);
        self.drop_dir = Some(self.app.drag_over(x as f32, y as f32));
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        self.drop_dir = None;
        self.app.drag_leave();
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, x: f64, y: f64) {
        if self.drop_dir.is_some() {
            self.drop_dir = Some(self.app.drag_over(x as f32, y as f32));
        }
    }

    /// Read the dropped list on a thread; the paste starts when it arrives
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        let Some(dir) = self.drop_dir.take() else { return };
        let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().drag_offer()) else { return };
        use wayland_client::protocol::wl_data_device_manager::DndAction;
        let move_allowed = offer.selected_action.contains(DndAction::Move);
        let Ok(pipe) = offer.receive(URI_LIST.into()) else { return };
        self.drop_offer = Some(offer);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut text = String::new();
            let mut reader = std::fs::File::from(std::os::fd::OwnedFd::from(pipe));
            let _ = reader.by_ref().take(1 << 20).read_to_string(&mut text);
            let _ = tx.send(Msg::Dropped(fsops::parse_uri_list(&text), dir, move_allowed));
        });
    }

    /// Files copied in another program become our paste source
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        if self.copy_source.is_some() {
            return;
        }
        let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().selection_offer()) else { return };
        if !offer.with_mime_types(|types| types.iter().any(|t| t == URI_LIST)) {
            return;
        }
        let Ok(pipe) = offer.receive(URI_LIST.into()) else { return };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut text = String::new();
            let mut reader = std::fs::File::from(std::os::fd::OwnedFd::from(pipe));
            if reader.by_ref().take(1 << 20).read_to_string(&mut text).is_ok() {
                let _ = tx.send(Msg::ExternalFiles(fsops::parse_uri_list(&text)));
            }
        });
    }
}

impl DataOfferHandler for Files {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

impl DataSourceHandler for Files {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}

    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource, mime: String, pipe: WritePipe) {
        let dragged = self.drag_source.as_ref().filter(|(s, _, _)| s.inner() == source).map(|(_, p, _)| p);
        let copied = self.copy_source.as_ref().filter(|(s, _)| s.inner() == source).map(|(_, p)| p);
        let Some(paths) = dragged.or(copied) else { return };
        let data = if mime == URI_LIST {
            fsops::uri_list(paths)
        } else {
            paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n")
        };
        // A slow reader must not block the window
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = std::fs::File::from(std::os::fd::OwnedFd::from(pipe)).write_all(data.as_bytes());
        });
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self.drag_source.as_ref().is_some_and(|(s, _, _)| s.inner() == source) {
            self.drag_source = None;
        }
        if self.copy_source.as_ref().is_some_and(|(s, _)| s.inner() == source) {
            self.copy_source = None;
        }
    }
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self.drag_source.as_ref().is_some_and(|(s, _, _)| s.inner() == source) {
            self.drag_source = None;
        }
    }
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

delegate_compositor!(Files);
delegate_output!(Files);
delegate_shm!(Files);
delegate_seat!(Files);
delegate_keyboard!(Files);
delegate_pointer!(Files);
delegate_xdg_shell!(Files);
delegate_xdg_window!(Files);
delegate_data_device!(Files);
delegate_registry!(Files);

impl ProvidesRegistryState for Files {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_list_round_trip() {
        let paths = vec![PathBuf::from("/home/u/Мой файл.txt"), PathBuf::from("/tmp/a&b")];
        let text = fsops::uri_list(&paths);
        assert!(text.starts_with("file:///home/u/%D0%9C"));
        assert_eq!(fsops::parse_uri_list(&text), paths);
        assert_eq!(fsops::parse_uri_list("# comment\nfile://host/x%20y\n"), [PathBuf::from("/x y")]);
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for Files {
    fn event(files: &mut Self, _: &WpFractionalScaleV1, event: wp_fractional_scale_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let scale = scale as f64 / 120.0;
            if files.fractional != Some(scale) {
                files.fractional = Some(scale);
                files.app.dirty = true;
            }
        }
    }
}

macro_rules! no_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for Files {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
no_events!(WpFractionalScaleManagerV1, WpViewporter, WpViewport);
