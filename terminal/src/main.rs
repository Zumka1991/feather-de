//! Терминал — Feather's terminal: own VT/xterm core, macOS Tahoe look, tiny-skia drawing
//! straight into shared memory. Nothing runs while idle: the pty, input and the
//! compositor's frame callbacks are the only things that wake it.

mod keys;
mod pty;
mod search;
mod view;
mod vt;

use std::{
    os::fd::BorrowedFd,
    path::PathBuf,
    time::{Duration, Instant},
};

use feather_ui::{draw as ui, scale as ui_scale, text::FontSystem};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    data_device_manager::{
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::{CopyPasteSource, DataSourceHandler},
        DataDeviceManagerState, WritePipe,
    },
    delegate_compositor, delegate_data_device, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            channel,
            generic::Generic,
            timer::{TimeoutAction, Timer},
            EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken,
        },
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
        wl_data_device::WlDataDevice, wl_data_device_manager::DndAction, wl_data_source::WlDataSource, wl_keyboard, wl_output, wl_pointer,
        wl_seat, wl_shm, wl_surface,
    },
    Connection, Dispatch, Proxy, QueueHandle,
};

use smithay_client_toolkit::reexports::protocols::xdg::system_bell::v1::client::xdg_system_bell_v1::XdgSystemBellV1;
use keys::{MouseEvent, Mods};
use pty::Pty;
use view::{Frame, Grid, Palette, SelRange};
use vt::{
    parser::Parser,
    term::{MouseMode, Term},
};

const TITLE_H: f32 = 30.0;
const TAB_H: f32 = 28.0;
const SEARCH_H: f32 = 34.0;
const PAD_X: f32 = 8.0;
const PAD_TOP: f32 = 4.0;
const PAD_BOTTOM: f32 = 6.0;
const RESIZE_BAND: f32 = 5.0;
const FONT_PT: f32 = 12.5;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const TEXT_MIMES: [&str; 5] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"];
const URI_LIST: &str = "text/uri-list";

struct Tab {
    id: u64,
    term: Term,
    parser: Parser,
    pty: Pty,
    source: Option<RegistrationToken>,
    /// Lines scrolled back from the live screen
    offset: usize,
    /// Value of `term.scrolled_out` the view was last drawn for
    seen_scrolled: u64,
    title: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SelMode {
    Char,
    Word,
    Line,
}

#[derive(Clone, Copy, Debug)]
struct Selection {
    anchor: (i64, usize),
    head: (i64, usize),
    mode: SelMode,
}

enum Msg {
    Paste(String),
}

struct App {
    registry_state: RegistryState,
    system_bell: Option<XdgSystemBellV1>,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    data_devices: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    copy_source: Option<(CopyPasteSource, String)>,
    pending_copy: Option<String>,
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
    /// Window size in logical points
    w: f32,
    h: f32,
    maximized: bool,
    focused: bool,
    tabs: Vec<Tab>,
    active: usize,
    next_tab: u64,
    grid: Grid,
    chrome_fonts: FontSystem,
    palette: Palette,
    font_pt: f32,
    canvas: Option<tiny_skia::Pixmap>,
    full_redraw: bool,
    /// Physical rectangles changed since the last frame
    damage: Vec<(i32, i32, i32, i32)>,
    selection: Option<Selection>,
    search: search::Search,
    search_timer: bool,
    search_reveal: bool,
    selecting: bool,
    last_click: Option<(Instant, (i64, usize), u8)>,
    pointer: (f32, f32),
    pointer_button: Option<u32>,
    hover: Option<Hover>,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    themed_pointer: Option<ThemedPointer>,
    cursor: Option<CursorIcon>,
    serial: u32,
    mods: Mods,
    /// Fractional wheel/touchpad distance not yet turned into whole lines
    scroll_accum: f32,
    handle: LoopHandle<'static, App>,
    qh: QueueHandle<App>,
    conn: Connection,
    tx: channel::Sender<Msg>,
    /// A frame is wanted (output, input, resize); drawn when the compositor allows
    needs_draw: bool,
    sync_timer: bool,
    first_frame: Option<Instant>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hover {
    Traffic(u8),
    Tab(usize),
    TabClose(usize),
    NewTab,
    SearchField,
    SearchPrevious,
    SearchNext,
    SearchClose,
}

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_alphanumeric() || "/._-+=:@%,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))).map(|b| b.join("feather-de"))
}

/// Font size chosen with Ctrl+= / Ctrl+− / Ctrl+wheel, kept between launches
fn saved_font_size() -> Option<f32> {
    let text = std::fs::read_to_string(config_dir()?.join("terminal.conf")).ok()?;
    text.lines().find_map(|l| l.trim().strip_prefix("font_size=")?.trim().parse::<f32>().ok()).map(|v| v.clamp(7.0, 36.0))
}

fn save_font_size(size: f32) {
    std::thread::spawn(move || {
        let Some(dir) = config_dir() else { return };
        let _ = std::fs::create_dir_all(&dir);
        let tmp = dir.join("terminal.conf.tmp");
        if std::fs::write(&tmp, format!("font_size={size}\n")).is_ok() {
            let _ = std::fs::rename(tmp, dir.join("terminal.conf"));
        }
    });
}

/// Appearance chosen in Feather's settings (dark/light), else light
fn system_dark() -> bool {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    base.and_then(|b| std::fs::read_to_string(b.join("feather-de/settings.conf")).ok())
        .is_some_and(|t| t.lines().any(|l| l.trim() == "appearance=dark"))
}

fn main() {
    let started = Instant::now();
    // Programs we open must not inherit an ignored SIGCHLD (see feather_ui::process)
    feather_ui::process::reap_children();
    let mut args = std::env::args().skip(1);
    let mut cwd: Option<PathBuf> = None;
    let mut command: Option<Vec<String>> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cwd" | "--workdir" => cwd = args.next().map(PathBuf::from),
            "-e" => {
                command = Some(args.by_ref().collect());
            }
            _ => {}
        }
    }

    let conn = Connection::connect_to_env().expect("Нет Wayland-композитора (WAYLAND_DISPLAY)");
    let (globals, queue) = registry_queue_init(&conn).expect("Реестр Wayland недоступен");
    let qh = queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("цикл событий");
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue).insert(handle.clone()).expect("источник Wayland");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let xdg = XdgShell::bind(&globals, &qh).expect("xdg_wm_base");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let surface = compositor.create_surface(&qh);
    let window = xdg.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_title("Терминал");
    window.set_app_id("feather-terminal");
    window.set_min_size(Some((360, 200)));
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    let fractional_manager: Option<WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
    let (viewport, fractional_object) = match (&viewporter, &fractional_manager) {
        (Some(v), Some(f)) => (Some(v.get_viewport(window.wl_surface(), &qh, ())), Some(f.get_fractional_scale(window.wl_surface(), &qh, ()))),
        _ => (None, None),
    };
    window.commit();

    // The shell starts now, while the compositor answers with the first configure
    let font_pt = saved_font_size().unwrap_or(FONT_PT);
    let grid = Grid::new(font_pt, 1.0);
    let (cw, ch) = grid.cell_px();
    let (cols, rows) = (80usize, 24usize);
    let w = cols as f32 * cw as f32 + PAD_X * 2.0;
    let h = TITLE_H + PAD_TOP + rows as f32 * ch as f32 + PAD_BOTTOM;

    let (tx, rx) = channel::channel::<Msg>();
    handle
        .insert_source(rx, |event, _, app| {
            if let channel::Event::Msg(Msg::Paste(text)) = event {
                app.paste_text(&text);
            }
        })
        .expect("канал");

    let rgba = shm.formats().contains(&wl_shm::Format::Abgr8888);
    let dark = system_dark();
    let mut app = App {
        registry_state: RegistryState::new(&globals),
        system_bell: globals.bind(&qh, 1..=1, ()).ok(),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        data_devices: DataDeviceManagerState::bind(&globals, &qh).ok(),
        data_device: None,
        copy_source: None,
        pending_copy: None,
        drop_offer: None,
        pool: SlotPool::new((w * h * 4.0) as usize, &shm).expect("пул wl_shm"),
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
        w,
        h,
        maximized: false,
        focused: true,
        tabs: Vec::new(),
        active: 0,
        next_tab: 0,
        grid,
        chrome_fonts: FontSystem::new(),
        palette: if dark { Palette::dark() } else { Palette::light() },
        font_pt,
        canvas: None,
        full_redraw: true,
        damage: Vec::new(),
        selection: None,
        search: search::Search::default(),
        search_timer: false,
        search_reveal: false,
        selecting: false,
        last_click: None,
        pointer: (0.0, 0.0),
        pointer_button: None,
        hover: None,
        seat: None,
        keyboard: None,
        themed_pointer: None,
        cursor: None,
        serial: 0,
        mods: Mods::default(),
        scroll_accum: 0.0,
        handle: handle.clone(),
        qh: qh.clone(),
        conn,
        tx,
        needs_draw: true,
        sync_timer: false,
        first_frame: std::env::var_os("FEATHER_PROFILE").map(|_| started),
    };
    app.open_tab(cwd, command.as_deref(), (cols, rows));
    app.install_debug_checks();
    while !app.exit {
        if event_loop.dispatch(None, &mut app).is_err() {
            break;
        }
        if app.needs_draw {
            app.draw();
        }
    }
}

impl App {
    fn install_debug_checks(&mut self) {
        let mode=std::env::var("FEATHER_TERMINAL_DEBUG").unwrap_or_default();
        let steps:Vec<(u64,u8)>=match mode.as_str(){
            "search"=>vec![(1200,0),(1800,1),(2300,2),(2800,3),(3400,4),(4000,5)],
            "copy"=>vec![(3000,6)],_=>return,
        };
        for (delay,step) in steps {
            let _=self.handle.insert_source(Timer::from_duration(Duration::from_millis(delay)),move|_,_,app|{
                let key=|app:&mut App,sym:Keysym,text:Option<&str>,mods:Mods|{
                    app.mods=mods;app.key(KeyEvent{time:0,raw_code:0,keysym:sym,utf8:text.map(str::to_string)});app.mods=Mods::default();
                };
                match step {
                    0=>{
                        key(app,Keysym::F,Some("F"),Mods{ctrl:true,shift:true,alt:false});
                        key(app,Keysym::p,Some("привет"),Mods::default());
                    }
                    1=>{assert_eq!(app.search.count,3);key(app,Keysym::Return,None,Mods::default());}
                    2=>{assert_eq!(app.search.index,0);assert!(app.tabs[app.active].offset>0);key(app,Keysym::Return,None,Mods{shift:true,..Default::default()});}
                    3=>{
                        assert_eq!(app.search.index,2);
                        key(app,Keysym::a,Some("a"),Mods{ctrl:true,..Default::default()});
                        key(app,Keysym::x,Some("no-such-result"),Mods::default());
                    }
                    4=>{
                        assert_eq!(app.search.count,0);
                        key(app,Keysym::Escape,None,Mods::default());assert!(!app.search.open);
                        key(app,Keysym::F,Some("F"),Mods{ctrl:true,shift:true,alt:false});
                        key(app,Keysym::p,Some("привет"),Mods::default());
                    }
                    5=>{assert_eq!(app.search.count,3);assert!(!app.search.busy());eprintln!("[feather-terminal-check] search, history, Unicode, next/previous, no results and Esc passed");}
                    _=>{
                        let (origin,_,_)=app.layout();let (cw,ch)=app.grid.cell_px();let s=app.scale_f();
                        let x=(origin.0+cw/2) as f32/s;let y=(origin.1+ch/2) as f32/s;
                        app.mods.shift=true;app.press(0x110,x,y,app.serial);
                        app.motion(x+12.0*cw as f32/s,y);app.release(0x110);
                        key(app,Keysym::C,Some("C"),Mods{ctrl:true,shift:true,alt:false});
                        assert_eq!(app.copy_source.as_ref().map(|(_,t)|t.as_str()),Some("terminal-copy"));
                        eprintln!("[feather-terminal-check] Shift mouse selection and Ctrl+Shift+C passed");
                    }
                }
                TimeoutAction::Drop
            });
        }
    }
    fn scale_f(&self) -> f32 {
        match (self.fractional, &self.viewport) {
            (Some(f), Some(_)) => f as f32,
            _ => self.scale.max(1) as f32,
        }
    }

    fn tab_h(&self) -> f32 {
        if self.tabs.len() > 1 { TAB_H } else { 0.0 }
    }

    /// Physical origin of the cell grid and the grid size that fits the window
    fn layout(&self) -> ((i32, i32), usize, usize) {
        let s = self.scale_f();
        let (cw, ch) = self.grid.cell_px();
        let chrome = self.chrome_h();
        let origin = ((PAD_X * s).round() as i32, ((chrome + PAD_TOP) * s).round() as i32);
        let cols = (((self.w - PAD_X * 2.0) * s) / cw as f32).floor().max(2.0) as usize;
        let rows = (((self.h - chrome - PAD_TOP - PAD_BOTTOM) * s) / ch as f32).floor().max(1.0) as usize;
        (origin, cols, rows)
    }

    fn tab(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    fn open_tab(&mut self, cwd: Option<PathBuf>, command: Option<&[String]>, size: (usize, usize)) {
        let pty = match Pty::spawn(size.0 as u16, size.1 as u16, cwd.as_deref(), command) {
            Ok(p) => p,
            Err(err) => {
                eprintln!("Терминал: не удалось запустить shell: {err}");
                if self.tabs.is_empty() {
                    self.exit = true;
                }
                return;
            }
        };
        self.next_tab += 1;
        let id = self.next_tab;
        let fd = pty.fd();
        // SAFETY: the fd belongs to the tab's Pty, which outlives this source (removed on close)
        let source = Generic::new(unsafe { BorrowedFd::borrow_raw(fd) }, Interest::READ, Mode::Level);
        let token = self.handle.insert_source(source, move |_, _, app| Ok(app.pty_ready(id))).ok();
        let mut term = Term::new(size.0, size.1);
        term.report_colors = (self.palette.fg, self.palette.bg);
        self.tabs.push(Tab { id, term, parser: Parser::new(), pty, source: token, offset: 0, seen_scrolled: 0, title: String::new() });
        self.active = self.tabs.len() - 1;
        self.selection = None;
        self.relayout();
        self.full_redraw = true;
        self.needs_draw = true;
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        if let Some(token) = tab.source {
            self.handle.remove(token);
        }
        drop(tab);
        if self.tabs.is_empty() {
            self.exit = true;
            return;
        }
        if self.active >= self.tabs.len() || self.active > index {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.selection = None;
        self.relayout();
        self.full_redraw = true;
        self.needs_draw = true;
    }

    /// Output of a tab's program: parse everything available (bounded per wake-up so
    /// input stays responsive during floods), answer queries, then ask for a frame
    fn pty_ready(&mut self, id: u64) -> PostAction {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else { return PostAction::Remove };
        let mut buf = [0u8; 65536];
        let mut total = 0;
        let mut closed = false;
        let mut retitled = false;
        let rang;
        let clipboard;
        {
            let tab = &mut self.tabs[index];
            while total < 4 << 20 {
                match tab.pty.read(&mut buf) {
                    Ok(0) => {
                        closed = true;
                        break;
                    }
                    Ok(n) => {
                        total += n;
                        tab.parser.advance(&mut tab.term, &buf[..n]);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        closed = true;
                        break;
                    }
                }
            }
            if !tab.term.responses.is_empty() {
                let replies = std::mem::take(&mut tab.term.responses);
                tab.pty.write(&replies);
            }
            let title = tab.term.title.clone().filter(|t| !t.is_empty()).or_else(|| tab.pty.foreground_name()).unwrap_or_default();
            if title != tab.title {
                tab.title = title;
                retitled = true;
            }
            rang = std::mem::take(&mut tab.term.bell) && index == self.active;
            clipboard = tab.term.clipboard.take().filter(|_| index == self.active);
        }
        if let Some(text) = clipboard { self.publish_copy(text); }
        if retitled {
            self.damage_chrome();
        }
        if rang {
            self.bell();
        }
        if closed {
            self.close_tab(index);
            return PostAction::Remove;
        }
        if index == self.active {
            if total > 0 && self.search.open { self.restart_search(false); self.full_redraw=true; }
            self.needs_draw = true;
        }
        PostAction::Continue
    }

    fn bell(&mut self) {
        if let Some(bell)=&self.system_bell {bell.ring(Some(self.window.wl_surface()));}
        self.damage_chrome();
    }

    fn damage_chrome(&mut self) {
        self.full_redraw = true;
        self.needs_draw = true;
    }

    /// Grid size follows the window, font and scale; every tab's program is told
    fn relayout(&mut self) {
        let s = self.scale_f();
        self.grid.set(self.font_pt, s);
        let (_, cols, rows) = self.layout();
        let (cw, ch) = self.grid.cell_px();
        for tab in &mut self.tabs {
            if (tab.term.cols(), tab.term.rows()) != (cols, rows) {
                tab.term.resize(cols, rows);
                tab.pty.resize(cols as u16, rows as u16, ((cols as i32 * cw) as u16, (rows as i32 * ch) as u16));
                tab.offset = tab.offset.min(tab.term.scrollback_len());
            }
        }
        self.full_redraw = true;
        self.needs_draw = true;
        if self.search.open { self.restart_search(false); }
    }

    fn write_to_active(&mut self, bytes: &[u8]) {
        if let Some(tab) = self.tab() {
            tab.pty.write(bytes);
            if tab.offset != 0 {
                tab.offset = 0;
                self.full_redraw = true;
            }
        }
    }

    fn paste_text(&mut self, text: &str) {
        if self.search.open {
            self.search.insert(text);
            self.restart_search(true);
            return;
        }
        let Some(tab) = self.tab() else { return };
        // Control characters that could end bracketed paste early are dropped
        let clean: String = text.chars().filter(|&c| c != '\x1b' && (c >= ' ' || c == '\n' || c == '\t' || c == '\r')).collect();
        let clean = clean.replace("\r\n", "\r").replace('\n', "\r");
        let mut bytes = Vec::new();
        if tab.term.modes.bracketed_paste {
            bytes.extend_from_slice(b"\x1b[200~");
            bytes.extend_from_slice(clean.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
        } else {
            bytes.extend_from_slice(clean.as_bytes());
        }
        self.write_to_active(&bytes);
    }

    // ---------- drawing ----------

    fn draw(&mut self) {
        if !self.configured || self.frame_pending || self.tabs.is_empty() {
            return;
        }
        let s = self.scale_f();
        // A program that asked for synchronized output gets up to 150 ms to finish its frame
        if self.tabs[self.active].term.modes.synchronized {
            if !self.sync_timer {
                self.sync_timer = true;
                let _ = self.handle.insert_source(Timer::from_duration(Duration::from_millis(150)), |_, _, app| {
                    app.sync_timer = false;
                    if let Some(tab) = app.tab() {
                        tab.term.modes.synchronized = false;
                    }
                    app.needs_draw = true;
                    TimeoutAction::Drop
                });
            }
            return;
        }
        self.needs_draw = false;
        let (w, h) = (self.w as i32, self.h as i32);
        let (pw, ph) = ((w as f32 * s).round() as i32, (h as f32 * s).round() as i32);
        if self.canvas.as_ref().is_none_or(|c| (c.width() as i32, c.height() as i32) != (pw, ph)) {
            self.canvas = tiny_skia::Pixmap::new(pw as u32, ph as u32);
            self.full_redraw = true;
        }
        let Some(mut canvas) = self.canvas.take() else { return };
        let (origin, _, _) = self.layout();
        let (_, ch) = self.grid.cell_px();
        {
            let mut pm = canvas.as_mut();
            let full = std::mem::take(&mut self.full_redraw);
            let tab = &mut self.tabs[self.active];
            // New output while scrolled back keeps the same lines in view
            let delta = tab.term.scrolled_out - tab.seen_scrolled;
            tab.seen_scrolled = tab.term.scrolled_out;
            let mut dirty = tab.term.take_dirty();
            if tab.offset > 0 && delta > 0 {
                tab.offset = (tab.offset + delta as usize).min(tab.term.scrollback_len());
                dirty.iter_mut().for_each(|d| *d = true);
            }
            let matches=self.search.visible_matches(&tab.term,tab.offset);
            if full {
                dirty.iter_mut().for_each(|d| *d = true);
                self.damage.push((0, 0, pw, ph));
            }
            let selection = self.selection.map(|sel| sel_range(&tab.term, sel));
            let frame = Frame {
                term: &tab.term,
                offset: tab.offset,
                palette: &self.palette,
                selection,
                focused: self.focused && !self.search.open,
                origin,
                cursor_on: true,
                matches: &matches,
                active_match: self.search.current.filter(|_|self.search.open),
            };
            if full {
                // Padding around the grid and below the last row
                view::fill(&mut pm, 0, 0, pw, ph, if tab.term.modes.reverse_video { self.palette.fg } else { self.palette.bg });
            }
            for (y, d) in dirty.iter().enumerate() {
                if *d {
                    self.grid.draw_row(&mut pm, &frame, y);
                    self.damage.push((0, origin.1 + y as i32 * ch, pw, ch));
                }
            }
            if full {
                self.paint_chrome(&mut pm, s);
            }
        }
        let format = if self.rgba { wl_shm::Format::Abgr8888 } else { wl_shm::Format::Argb8888 };
        let Ok((buffer, shm)) = self.pool.create_buffer(pw, ph, pw * 4, format) else {
            self.canvas = Some(canvas);
            return;
        };
        ui::copy_shm_pixels(shm, canvas.data(), self.rgba);
        self.canvas = Some(canvas);
        let surface = self.window.wl_surface().clone();
        match &self.viewport {
            Some(viewport) if self.fractional.is_some() => {
                surface.set_buffer_scale(1);
                viewport.set_destination(w, h);
            }
            _ => surface.set_buffer_scale(s as i32),
        }
        for (x, y, dw, dh) in self.damage.drain(..) {
            surface.damage_buffer(x, y, dw, dh);
        }
        surface.frame(&self.qh, surface.clone());
        self.window.set_window_geometry(0, 0, w as u32, h as u32);
        if buffer.attach_to(&surface).is_err() {
            return;
        }
        self.window.commit();
        self.frame_pending = true;
        if let Some(t) = self.first_frame.take() {
            eprintln!("[feather-terminal] первый кадр через {:.1} мс после запуска", t.elapsed().as_secs_f64() * 1e3);
        }
    }

    /// Title bar with traffic lights and the title, and the tab strip when there are tabs
    fn paint_chrome(&mut self, pm: &mut tiny_skia::PixmapMut, s: f32) {
        let dark = self.palette.dark;
        let (bar, text, sub, line) = if dark {
            ([44, 44, 48], ui::rgba(230, 230, 235, 255), ui::rgba(150, 150, 158, 255), [20, 20, 22])
        } else {
            ([238, 238, 241], ui::rgba(40, 40, 44, 255), ui::rgba(120, 120, 128, 255), [212, 212, 216])
        };
        let total = TITLE_H + self.tab_h();
        view::fill(pm, 0, 0, pm.width() as i32, (total * s).round() as i32, bar);
        view::fill(pm, 0, (total * s).round() as i32 - (s.round() as i32).max(1), pm.width() as i32, (s.round() as i32).max(1), line);
        ui_scale::with_render_settings(s, if dark { ui_scale::Appearance::Dark } else { ui_scale::Appearance::Light }, || {
            let hovered = matches!(self.hover, Some(Hover::Traffic(_)));
            for (i, cx) in [20.0f32, 40.0, 60.0].into_iter().enumerate() {
                let color = if !self.focused && !hovered {
                    if dark { ui::rgba(80, 80, 86, 255) } else { ui::rgba(205, 205, 210, 255) }
                } else {
                    [ui::rgba(255, 95, 87, 255), ui::rgba(254, 188, 46, 255), ui::rgba(40, 200, 64, 255)][i]
                };
                ui::fill_circle(pm, cx, 15.0, 6.0, color);
                if hovered {
                    let ink = ui::rgba(0, 0, 0, 140);
                    match i {
                        0 => {
                            ui::line(pm, cx - 2.5, 12.5, cx + 2.5, 17.5, 1.2, ink);
                            ui::line(pm, cx + 2.5, 12.5, cx - 2.5, 17.5, 1.2, ink);
                        }
                        1 => ui::line(pm, cx - 3.0, 15.0, cx + 3.0, 15.0, 1.2, ink),
                        _ => {
                            ui::line(pm, cx - 3.0, 15.0, cx + 3.0, 15.0, 1.2, ink);
                            ui::line(pm, cx, 12.0, cx, 18.0, 1.2, ink);
                        }
                    }
                }
            }
            let (cols, rows) = self.tabs.get(self.active).map(|t| (t.term.cols(), t.term.rows())).unwrap_or((0, 0));
            let name = self.tabs.get(self.active).map(|t| t.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "Терминал".into());
            let title = format!("{name} — {cols}×{rows}");
            let title = self.chrome_fonts.ellipsize(&title, self.w - 200.0, 13.0, true);
            self.chrome_fonts.draw_text_centered(pm, &title, self.w * 0.5, 7.5, 13.0, text, true);
            if self.tabs.len() > 1 {
                let tw = (self.w - 40.0) / self.tabs.len() as f32;
                for (i, tab) in self.tabs.iter().enumerate() {
                    let x = i as f32 * tw;
                    let active = i == self.active;
                    if active {
                        let bg = self.palette.bg;
                        ui::fill_rrect(pm, x + 4.0, TITLE_H + 2.0, tw - 8.0, TAB_H - 6.0, 6.0, ui::rgba(bg[0], bg[1], bg[2], 255));
                    } else if self.hover == Some(Hover::Tab(i)) || self.hover == Some(Hover::TabClose(i)) {
                        ui::fill_rrect(pm, x + 4.0, TITLE_H + 2.0, tw - 8.0, TAB_H - 6.0, 6.0, ui::rgba(0, 0, 0, 18));
                    }
                    let label = if tab.title.is_empty() { "Терминал".to_string() } else { tab.title.clone() };
                    let label = self.chrome_fonts.ellipsize(&label, tw - 44.0, 12.0, false);
                    self.chrome_fonts.draw_text_centered(pm, &label, x + tw * 0.5, TITLE_H + 6.0, 12.0, if active { text } else { sub }, false);
                    if matches!(self.hover, Some(Hover::Tab(j)) | Some(Hover::TabClose(j)) if j == i) {
                        let (cx, cy) = (x + 18.0, TITLE_H + 12.0);
                        if self.hover == Some(Hover::TabClose(i)) {
                            ui::fill_circle(pm, cx, cy, 8.0, ui::rgba(0, 0, 0, 30));
                        }
                        ui::line(pm, cx - 3.0, cy - 3.0, cx + 3.0, cy + 3.0, 1.3, sub);
                        ui::line(pm, cx + 3.0, cy - 3.0, cx - 3.0, cy + 3.0, 1.3, sub);
                    }
                }
                let (px, py) = (self.w - 20.0, TITLE_H + 12.0);
                if self.hover == Some(Hover::NewTab) {
                    ui::fill_circle(pm, px, py, 10.0, ui::rgba(0, 0, 0, 22));
                }
                ui::line(pm, px - 5.0, py, px + 5.0, py, 1.4, sub);
                ui::line(pm, px, py - 5.0, px, py + 5.0, 1.4, sub);
            }
        });
        if self.search.open { self.paint_search(pm,s); }
    }

    fn chrome_h(&self) -> f32 { TITLE_H+self.tab_h()+if self.search.open {SEARCH_H}else{0.0} }

    fn open_search(&mut self) {
        let was_open=self.search.open;
        if !was_open && self.search.query.is_empty(){if let Some(text)=self.selected_text(){self.search.set_query(&text);}}
        self.search.open=true;self.search.select_all=true;
        if !was_open {self.relayout();}
        self.restart_search(true);
    }

    fn close_search(&mut self) {
        self.search.close();self.relayout();
    }

    fn restart_search(&mut self, reveal: bool) {
        let Some(tab)=self.tabs.get(self.active) else{return};
        let anchor=self.search.current.map(|s|s.start).unwrap_or((tab.term.scrolled_out as i64-tab.offset as i64,0));
        self.search.restart(&tab.term,anchor);
        self.search_reveal=reveal;
        self.arm_search();self.damage_chrome();
    }

    fn navigate_search(&mut self, previous: bool) {
        let Some(tab)=self.tabs.get(self.active) else{return};
        self.search.navigate(&tab.term,previous);
        self.search_reveal=true;self.arm_search();self.damage_chrome();
    }

    fn arm_search(&mut self) {
        if self.search_timer || !self.search.busy(){return;}
        self.search_timer=true;
        let inserted=self.handle.insert_source(Timer::from_duration(Duration::ZERO),|_,_,app|{
            let done=app.tabs.get(app.active).is_none_or(|t|app.search.step(&t.term,4096));
            if !done {return TimeoutAction::ToDuration(Duration::ZERO);}
            app.search_timer=false;
            if app.search_reveal {
                if let (Some(hit),Some(tab))=(app.search.current,app.tabs.get_mut(app.active)) {
                    let top=tab.term.scrolled_out as i64-tab.offset as i64;
                    if hit.start.0<top || hit.end.0>=top+tab.term.rows() as i64 {
                        let top=(hit.start.0-tab.term.rows() as i64/2).clamp(tab.term.first_abs(),tab.term.scrolled_out as i64);
                        tab.offset=(tab.term.scrolled_out as i64-top).max(0) as usize;
                    }
                }
            }
            app.damage_chrome();TimeoutAction::Drop
        });
        if inserted.is_err(){self.search_timer=false;}
    }

    fn search_key(&mut self, sym: Keysym, text: Option<&str>, mods: Mods) {
        match sym {
            Keysym::Escape => return self.close_search(),
            Keysym::Return | Keysym::KP_Enter | Keysym::F3 => return self.navigate_search(mods.shift),
            Keysym::Left => {self.search.move_caret(false);self.damage_chrome();return;}
            Keysym::Right => {self.search.move_caret(true);self.damage_chrome();return;}
            Keysym::Home => {self.search.caret=0;self.search.select_all=false;self.damage_chrome();return;}
            Keysym::End => {self.search.caret=self.search.query.len();self.search.select_all=false;self.damage_chrome();return;}
            Keysym::BackSpace => self.search.backspace(false),
            Keysym::Delete => self.search.backspace(true),
            _ if mods.ctrl => match latin_letter(sym) {
                Some('a') => {self.search.select_all=true;self.damage_chrome();return;}
                Some('v') => return self.paste_clipboard(),
                Some('c') if self.search.select_all => return self.publish_copy(self.search.query.clone()),
                Some('g') => return self.navigate_search(mods.shift),
                _ => return,
            },
            _ if !mods.alt => {if let Some(text)=text{self.search.insert(text);}else{return;}}
            _ => return,
        }
        self.restart_search(true);
    }

    fn paint_search(&mut self, pm: &mut tiny_skia::PixmapMut, s: f32) {
        let y=TITLE_H+self.tab_h();
        let dark=self.palette.dark;
        let bg=if dark {[44,44,48]}else{[238,238,241]};
        view::fill(pm,0,(y*s).round() as i32,pm.width() as i32,(SEARCH_H*s).round() as i32,bg);
        ui_scale::with_render_settings(s,if dark{ui_scale::Appearance::Dark}else{ui_scale::Appearance::Light},||{
            let text=if dark{ui::rgba(230,230,235,255)}else{ui::rgba(40,40,44,255)};
            let sub=if dark{ui::rgba(150,150,158,255)}else{ui::rgba(120,120,128,255)};
            let field_w=(self.w-188.0).max(110.0);
            ui::fill_rrect(pm,8.0,y+4.0,field_w,26.0,6.0,if dark{ui::rgba(25,25,28,255)}else{ui::rgba(255,255,255,255)});
            ui::stroke_rrect(pm,8.0,y+4.0,field_w,26.0,6.0,1.0,ui::rgba(70,145,245,255));
            let max_w=field_w-16.0;
            let mut start=0;
            while start<self.search.caret && self.chrome_fonts.measure_text(&self.search.query[start..self.search.caret],12.0,false)>max_w-2.0 {
                start+=self.search.query[start..].chars().next().unwrap().len_utf8();
            }
            let visible=self.chrome_fonts.ellipsize(&self.search.query[start..],max_w,12.0,false);
            let shown=if self.search.query.is_empty(){"Найти…"}else{&visible};
            if self.search.select_all && !self.search.query.is_empty(){
                let width=self.chrome_fonts.measure_text(shown,12.0,false);
                ui::fill_rrect(pm,15.0,y+7.0,width+2.0,19.0,2.0,ui::rgba(65,140,240,180));
            }
            self.chrome_fonts.draw_text(pm,shown,16.0,y+8.0,12.0,if self.search.query.is_empty(){sub}else{text},false);
            if !self.search.select_all {
                let x=16.0+self.chrome_fonts.measure_text(&self.search.query[start..self.search.caret],12.0,false);
                ui::line(pm,x,y+8.0,x,y+24.0,1.0,text);
            }
            let counter=if self.search.busy(){"…".into()}else if self.search.query.is_empty(){String::new()}
                else if self.search.count==0{"Нет совпадений".into()}else{format!("{}/{}",self.search.index+1,self.search.count)};
            let counter=self.chrome_fonts.ellipsize(&counter,(self.w-field_w-106.0).max(60.0),10.0,false);
            self.chrome_fonts.draw_text(pm,&counter,field_w+16.0,y+10.0,10.0,sub,false);
            for (hover,cx,previous) in [(Hover::SearchPrevious,self.w-74.0,true),(Hover::SearchNext,self.w-46.0,false)] {
                if self.hover==Some(hover){ui::fill_rrect(pm,cx-11.0,y+5.0,22.0,24.0,5.0,ui::rgba(0,0,0,24));}
                let dy=if previous{-3.0}else{3.0};
                ui::line(pm,cx-4.0,y+17.0-dy,cx,y+17.0+dy,1.3,sub);
                ui::line(pm,cx,y+17.0+dy,cx+4.0,y+17.0-dy,1.3,sub);
            }
            let cx=self.w-18.0;
            if self.hover==Some(Hover::SearchClose){ui::fill_circle(pm,cx,y+17.0,10.0,ui::rgba(0,0,0,24));}
            ui::line(pm,cx-3.0,y+14.0,cx+3.0,y+20.0,1.3,sub);
            ui::line(pm,cx+3.0,y+14.0,cx-3.0,y+20.0,1.3,sub);
        });
    }

    // ---------- input ----------

    fn chrome_hit(&self, x: f32, y: f32) -> Option<Hover> {
        let search_y=TITLE_H+self.tab_h();
        if self.search.open && y>=search_y && y<search_y+SEARCH_H {
            return Some(if x>=self.w-32.0 {Hover::SearchClose} else if x>=self.w-60.0 {Hover::SearchNext}
                else if x>=self.w-88.0 {Hover::SearchPrevious} else {Hover::SearchField});
        }
        if y < TITLE_H {
            for (i, cx) in [20.0f32, 40.0, 60.0].into_iter().enumerate() {
                if (x - cx).abs() <= 8.0 && (y - 15.0).abs() <= 8.0 {
                    return Some(Hover::Traffic(i as u8));
                }
            }
            return None;
        }
        if self.tabs.len() > 1 && y < TITLE_H + TAB_H {
            if x >= self.w - 40.0 {
                return Some(Hover::NewTab);
            }
            let tw = (self.w - 40.0) / self.tabs.len() as f32;
            let i = ((x / tw) as usize).min(self.tabs.len() - 1);
            let close = (x - (i as f32 * tw + 18.0)).abs() <= 8.0;
            return Some(if close { Hover::TabClose(i) } else { Hover::Tab(i) });
        }
        None
    }

    fn resize_edges(&self, x: f32, y: f32) -> u32 {
        if self.maximized {
            return 0;
        }
        let mut e = 0;
        if y < RESIZE_BAND { e |= 1 }
        if y >= self.h - RESIZE_BAND { e |= 2 }
        if x < RESIZE_BAND { e |= 4 }
        if x >= self.w - RESIZE_BAND { e |= 8 }
        e
    }

    /// Cell under a logical point, as an absolute line and column
    fn cell_under(&self, x: f32, y: f32) -> (i64, usize, usize, usize) {
        let s = self.scale_f();
        let (origin, _, _) = self.layout();
        let tab = &self.tabs[self.active];
        let (cx, cy) = view::cell_at(x * s, y * s, origin, self.grid.cell_px(), tab.term.cols(), tab.term.rows());
        let abs = tab.term.scrolled_out as i64 - tab.offset as i64 + cy as i64;
        (abs, cx, cx, cy)
    }

    fn in_grid(&self, y: f32) -> bool {
        y >= self.chrome_h()
    }

    fn mouse_reporting(&self) -> bool {
        !self.search.open && !self.mods.shift && self.tabs.get(self.active).is_some_and(|t| t.term.modes.mouse != MouseMode::Off)
    }

    fn report_mouse(&mut self, event: MouseEvent) {
        let (x, y) = self.pointer;
        let (_, col, _, row) = self.cell_under(x, y);
        let (mods, sgr) = (self.mods, self.tabs[self.active].term.modes.mouse_sgr);
        let bytes = keys::mouse_report(event, col, row, mods, sgr);
        self.tabs[self.active].pty.write(&bytes);
    }

    fn press(&mut self, button: u32, x: f32, y: f32, serial: u32) {
        self.serial = serial;
        let edges = self.resize_edges(x, y);
        if button == 0x110 && edges != 0 {
            use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge as E;
            let edge = match edges { 1 => E::Top, 2 => E::Bottom, 4 => E::Left, 8 => E::Right, 5 => E::TopLeft, 9 => E::TopRight, 6 => E::BottomLeft, 10 => E::BottomRight, _ => E::None };
            if let Some(seat) = &self.seat {
                self.window.resize(seat, serial, edge);
            }
            return;
        }
        if !self.in_grid(y) {
            match (button, self.chrome_hit(x, y)) {
                (0x110, Some(Hover::Traffic(0))) => self.exit = true,
                (0x110, Some(Hover::Traffic(1))) => self.window.set_minimized(),
                (0x110, Some(Hover::Traffic(2))) => self.toggle_maximize(),
                (0x110, Some(Hover::NewTab)) => self.new_tab(),
                (0x110, Some(Hover::SearchClose)) => self.close_search(),
                (0x110, Some(Hover::SearchPrevious)) => self.navigate_search(true),
                (0x110, Some(Hover::SearchNext)) => self.navigate_search(false),
                (0x110, Some(Hover::SearchField)) => { self.search.select_all=true; self.damage_chrome(); },
                (0x110, Some(Hover::TabClose(i))) | (0x112, Some(Hover::Tab(i) | Hover::TabClose(i))) => self.close_tab(i),
                (0x110, Some(Hover::Tab(i))) => self.switch_tab(i),
                (0x110, None) => {
                    let now = Instant::now();
                    if self.last_click.is_some_and(|(t, c, _)| c == (-1, 0) && now.duration_since(t) < DOUBLE_CLICK) {
                        self.last_click = None;
                        self.toggle_maximize();
                    } else {
                        self.last_click = Some((now, (-1, 0), 1));
                        if let Some(seat) = &self.seat {
                            self.window.move_(seat, serial);
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        if self.mouse_reporting() && !self.search.open {
            let b = match button { 0x110 => 0, 0x112 => 1, 0x111 => 2, _ => return };
            self.pointer_button = Some(button);
            self.report_mouse(MouseEvent::Press(b));
            return;
        }
        match button {
            0x110 => {
                let (abs, col, _, _) = self.cell_under(x, y);
                let now = Instant::now();
                let count = match self.last_click {
                    Some((t, cell, n)) if cell == (abs, col) && now.duration_since(t) < DOUBLE_CLICK => n % 3 + 1,
                    _ => 1,
                };
                self.last_click = Some((now, (abs, col), count));
                let mode = [SelMode::Char, SelMode::Word, SelMode::Line][count as usize - 1];
                if self.mods.shift && self.selection.is_some() {
                    if let Some(sel) = self.selection.as_mut() {
                        sel.head = (abs, col);
                    }
                } else {
                    self.selection = Some(Selection { anchor: (abs, col), head: (abs, col), mode });
                }
                self.selecting = true;
                self.pointer_button = Some(button);
                // A single click without a drag selects nothing
                if mode == SelMode::Char && !self.mods.shift {
                    self.selection = Some(Selection { anchor: (abs, col), head: (abs, col), mode });
                }
                self.full_redraw = true;
                self.needs_draw = true;
            }
            0x112 => self.paste_clipboard(),
            _ => {}
        }
    }

    fn release(&mut self, button: u32) {
        if self.mouse_reporting() && self.pointer_button == Some(button) {
            let b = match button { 0x110 => 0, 0x112 => 1, 0x111 => 2, _ => 0 };
            self.report_mouse(MouseEvent::Release(b));
        }
        self.pointer_button = None;
        if self.selecting {
            self.selecting = false;
            // A plain click (no drag) clears the selection
            if let Some(sel) = self.selection {
                if sel.mode == SelMode::Char && sel.anchor == sel.head {
                    self.selection = None;
                    self.full_redraw = true;
                    self.needs_draw = true;
                }
            }
        }
    }

    fn motion(&mut self, x: f32, y: f32) {
        self.pointer = (x, y);
        let hover = if self.in_grid(y) { None } else { self.chrome_hit(x, y) };
        if hover != self.hover {
            self.hover = hover;
            self.damage_chrome();
        }
        if self.mouse_reporting() && self.in_grid(y) {
            let mode = self.tabs[self.active].term.modes.mouse;
            match (mode, self.pointer_button) {
                (MouseMode::Drag | MouseMode::Motion, Some(b)) => {
                    let b = match b { 0x110 => 0, 0x112 => 1, _ => 2 };
                    self.report_mouse(MouseEvent::Motion(b));
                }
                (MouseMode::Motion, None) => self.report_mouse(MouseEvent::Motion(3)),
                _ => {}
            }
            return;
        }
        if self.selecting {
            let (abs, col, _, _) = self.cell_under(x, y);
            if let Some(sel) = self.selection.as_mut() {
                if sel.head != (abs, col) {
                    sel.head = (abs, col);
                    self.full_redraw = true;
                    self.needs_draw = true;
                }
            }
        }
    }

    fn scroll(&mut self, lines: f32) {
        if self.tabs.is_empty() {
            return;
        }
        if self.mouse_reporting() {
            let ev = if lines < 0.0 { MouseEvent::WheelUp } else { MouseEvent::WheelDown };
            self.report_mouse(ev);
            return;
        }
        self.scroll_accum += lines;
        let whole = self.scroll_accum.trunc();
        self.scroll_accum -= whole;
        let n = whole as i64;
        if n == 0 {
            return;
        }
        let tab = &mut self.tabs[self.active];
        if tab.term.alt_active && tab.term.modes.alternate_scroll && !self.search.open {
            // less, man, vim: the wheel scrolls as arrow keys
            let key: &[u8] = match (n < 0, tab.term.modes.cursor_keys_app) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            let bytes = key.repeat(n.unsigned_abs() as usize);
            tab.pty.write(&bytes);
            return;
        }
        let max = tab.term.scrollback_len() as i64;
        let new = (tab.offset as i64 - n).clamp(0, max) as usize;
        if new != tab.offset {
            tab.offset = new;
            self.full_redraw = true;
            self.needs_draw = true;
        }
    }

    fn toggle_maximize(&mut self) {
        if self.maximized { self.window.unset_maximized() } else { self.window.set_maximized() }
    }

    fn new_tab(&mut self) {
        let cwd = self.tabs.get(self.active).and_then(|t| t.term.cwd.clone().map(PathBuf::from).or_else(|| t.pty.cwd()));
        let (_, cols, rows) = self.layout();
        // The strip appears with the second tab, so the grid gets shorter
        self.open_tab(cwd, None, (cols, rows));
    }

    fn switch_tab(&mut self, i: usize) {
        if i < self.tabs.len() && i != self.active {
            self.active = i;
            self.selection = None;
            self.full_redraw = true;
            self.needs_draw = true;
            if self.search.open {self.restart_search(false);}
        }
    }

    fn zoom(&mut self, delta: f32) {
        let size = if delta == 0.0 { FONT_PT } else { (self.font_pt + delta).clamp(7.0, 36.0) };
        if size != self.font_pt {
            self.font_pt = size;
            save_font_size(size);
            self.relayout();
            self.damage_chrome();
        }
    }

    fn selected_text(&self) -> Option<String> {
        let sel = self.selection?;
        let tab = self.tabs.get(self.active)?;
        let r = sel_range(&tab.term, sel);
        let text = tab.term.text_between(r.start, r.end);
        (!text.is_empty()).then_some(text)
    }

    fn copy(&mut self) {
        if self.search.open && self.search.select_all {self.publish_copy(self.search.query.clone());return;}
        let Some(text) = self.selected_text() else { return };
        self.publish_copy(text);
    }

    fn publish_copy(&mut self, text: String) {
        if !self.focused || self.serial == 0 {
            self.pending_copy = Some(text);
            return;
        }
        let (Some(manager), Some(device)) = (self.data_devices.as_ref(), self.data_device.as_ref()) else { return };
        let source = manager.create_copy_paste_source(&self.qh, TEXT_MIMES);
        source.set_selection(device, self.serial);
        self.copy_source = Some((source, text));
    }

    /// Our own copy pastes directly; another program's is read on a thread.
    /// Files copied in a file manager paste as shell-quoted paths.
    fn paste_clipboard(&mut self) {
        if let Some((_, text)) = &self.copy_source {
            let text = text.clone();
            self.paste_text(&text);
            return;
        }
        let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().selection_offer()) else { return };
        let mimes = offer.with_mime_types(|m| m.to_vec());
        let (mime, files) = match TEXT_MIMES.iter().find(|t| mimes.iter().any(|m| m == *t)) {
            Some(t) if !mimes.iter().any(|m| m == URI_LIST) => (t.to_string(), false),
            _ if mimes.iter().any(|m| m == URI_LIST) => (URI_LIST.to_string(), true),
            Some(t) => (t.to_string(), false),
            None => return,
        };
        let Ok(pipe) = offer.receive(mime) else { return };
        read_paste(pipe.into(), files, self.tx.clone());
    }

    fn key(&mut self, event: KeyEvent) {
        let m = self.mods;
        let sym = event.keysym;
        let text = event.utf8.as_deref();
        let lower = text.map(|t| t.to_lowercase());
        let letter = lower.as_deref();
        // Terminal shortcuts (Konsole's): Ctrl+Shift+letter, so programs keep Ctrl+letter
        if m.ctrl && m.shift {
            let latin = latin_letter(sym);
            match latin {
                Some('c') => return self.copy(),
                Some('f') => return self.open_search(),
                Some('v') => return self.paste_clipboard(),
                Some('t') => return self.new_tab(),
                Some('w') => return self.close_tab(self.active),
                Some('n') => {
                    if let Ok(exe) = std::env::current_exe() {
                        let mut cmd = std::process::Command::new(exe);
                        if let Some(dir) = self.tabs.get(self.active).and_then(|t| t.pty.cwd()) {
                            cmd.arg("--cwd").arg(dir);
                        }
                        let _ = cmd.spawn();
                    }
                    return;
                }
                Some('k') => {
                    if let Some(tab) = self.tab() {
                        tab.parser.advance(&mut tab.term, b"\x1b[3J");
                        tab.offset = 0;
                    }
                    self.full_redraw = true;
                    self.needs_draw = true;
                    if self.search.open {self.restart_search(false);}
                    return;
                }
                _ => {}
            }
        }
        if sym==Keysym::F3 && !self.search.open { self.open_search(); return; }
        if m.ctrl && !m.alt {
            match sym {
                Keysym::Page_Up => return self.switch_tab((self.active + self.tabs.len() - 1) % self.tabs.len().max(1)),
                Keysym::Page_Down => return self.switch_tab((self.active + 1) % self.tabs.len().max(1)),
                _ => {}
            }
            match letter {
                Some("=") | Some("+") => return self.zoom(1.0),
                Some("-") => return self.zoom(-1.0),
                Some("0") => return self.zoom(0.0),
                _ => {}
            }
        }
        if self.search.open { self.search_key(sym,text,m); return; }
        if m.shift && !m.ctrl {
            let page = self.tabs.get(self.active).map_or(1.0, |t| t.term.rows() as f32 - 1.0);
            match sym {
                Keysym::Page_Up => return self.scroll(-page),
                Keysym::Page_Down => return self.scroll(page),
                Keysym::Home => return self.scroll(-1e9),
                Keysym::End => return self.scroll(1e9),
                _ => {}
            }
        }
        let Some(tab) = self.tabs.get(self.active) else { return };
        if let Some(bytes) = keys::encode(sym, text, m, &tab.term.modes) {
            self.write_to_active(&bytes);
        }
    }
}

/// Latin letter of a key in any layout (Ctrl+Shift+С copies in the Russian layout too)
fn latin_letter(sym: Keysym) -> Option<char> {
    let c = sym.key_char()?.to_lowercase().next()?;
    if c.is_ascii_lowercase() {
        return Some(c);
    }
    const RU: &str = "йцукенгшщзхъфывапролджэячсмитьбю";
    const EN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.";
    RU.chars().position(|r| r == c).and_then(|i| EN.chars().nth(i))
}

fn read_paste(fd: std::os::fd::OwnedFd, files: bool, tx: channel::Sender<Msg>) {
    std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        let _ = std::fs::File::from(fd).take(8 << 20).read_to_string(&mut text);
        let text = if files {
            feather_ui::fileops::parse_uri_list(&text).iter().map(|p| shell_quote(&p.to_string_lossy())).collect::<Vec<_>>().join(" ") + " "
        } else {
            text
        };
        let _ = tx.send(Msg::Paste(text));
    });
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || "_-./~:@%+=".contains(c)
}

/// Normalised selection range; words and lines snap to their edges
fn sel_range(term: &Term, sel: Selection) -> SelRange {
    let (mut a, mut b) = (sel.anchor.min(sel.head), sel.anchor.max(sel.head));
    match sel.mode {
        SelMode::Char => {}
        SelMode::Line => {
            a.1 = 0;
            b.1 = term.cols() - 1;
            // Whole wrapped lines
            while term.abs_row(a.0 - 1).is_some_and(|r| r.wrapped) {
                a.0 -= 1;
            }
            while term.abs_row(b.0).is_some_and(|r| r.wrapped) {
                b.0 += 1;
            }
        }
        SelMode::Word => {
            let ch = |abs: i64, x: usize| term.abs_row(abs).and_then(|r| r.cells.get(x)).map_or(' ', |c| c.ch);
            if is_word(ch(a.0, a.1)) {
                while a.1 > 0 && is_word(ch(a.0, a.1 - 1)) {
                    a.1 -= 1;
                }
            }
            if is_word(ch(b.0, b.1)) {
                while b.1 + 1 < term.cols() && is_word(ch(b.0, b.1 + 1)) {
                    b.1 += 1;
                }
            }
        }
    }
    SelRange { start: a, end: b }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, factor: i32) {
        if factor != self.scale {
            self.scale = factor;
            self.relayout();
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, configure: WindowConfigure, _: u32) {
        let w = configure.new_size.0.map_or(self.w, |v| v.get() as f32).max(360.0);
        let h = configure.new_size.1.map_or(self.h, |v| v.get() as f32).max(200.0);
        // Only real changes repaint: a configure that repeats the state must not start a
        // frame (compositors may answer each commit with a configure)
        let maximized = configure.state.contains(WindowState::MAXIMIZED) || configure.state.contains(WindowState::FULLSCREEN);
        if maximized != self.maximized {
            self.maximized = maximized;
            self.needs_draw = true;
        }
        let focused = configure.state.contains(WindowState::ACTIVATED);
        if focused != self.focused {
            self.focused = focused;
            if focused {if let Some(text)=self.pending_copy.take(){self.publish_copy(text);}}
            self.full_redraw = true;
            self.needs_draw = true;
            if let Some(tab) = self.tabs.get(self.active) {
                if tab.term.modes.focus_events {
                    tab.pty.write(if focused { b"\x1b[I" } else { b"\x1b[O" });
                }
            }
        }
        if (w, h) != (self.w, self.h) || !self.configured {
            self.w = w;
            self.h = h;
            self.relayout();
        }
        self.configured = true;
    }
}

impl SeatHandler for App {
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
            let keyboard = self.seat_state.get_keyboard_with_repeat(qh, &seat, None, self.handle.clone(), Box::new(|app, _, event| app.key(event)));
            self.keyboard = keyboard.ok();
        }
        if capability == Capability::Pointer && self.themed_pointer.is_none() {
            let surface = self.compositor.create_surface(qh);
            self.themed_pointer = self.seat_state.get_pointer_with_theme(qh, &seat, self.shm.wl_shm(), surface, ThemeSpec::default()).ok();
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if capability == Capability::Pointer {
            self.themed_pointer = None;
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, serial: u32, _: &[u32], _: &[Keysym]) {
        self.serial = serial;
        if let Some(text) = self.pending_copy.take() { self.publish_copy(text); }
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

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for event in events {
            if &event.surface != self.window.wl_surface() {
                continue;
            }
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            match event.kind {
                PointerEventKind::Enter { .. } => {
                    self.cursor = None;
                    self.motion(x, y);
                }
                PointerEventKind::Leave { .. } => {
                    if self.hover.take().is_some() {
                        self.damage_chrome();
                    }
                }
                PointerEventKind::Motion { .. } => self.motion(x, y),
                PointerEventKind::Press { button, serial, .. } => self.press(button, x, y, serial),
                PointerEventKind::Release { button, .. } => self.release(button),
                PointerEventKind::Axis { vertical, .. } if self.mods.ctrl => {
                    // Ctrl+wheel zooms, one point per notch (touchpads: per 40 px)
                    let steps = if vertical.discrete != 0 { -vertical.discrete as f32 } else { -vertical.absolute as f32 / 40.0 };
                    self.scroll_accum += steps;
                    let whole = self.scroll_accum.trunc();
                    self.scroll_accum -= whole;
                    if whole != 0.0 {
                        self.zoom(whole);
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let (_, ch) = self.grid.cell_px();
                    let lines = if vertical.discrete != 0 { vertical.discrete as f32 * 3.0 } else { vertical.absolute as f32 * self.scale_f() / ch as f32 };
                    self.scroll(lines);
                }
            }
        }
        let (x, y) = self.pointer;
        let edges = self.resize_edges(x, y);
        let wanted = match edges {
            1 => CursorIcon::NResize,
            2 => CursorIcon::SResize,
            4 => CursorIcon::WResize,
            8 => CursorIcon::EResize,
            5 => CursorIcon::NwResize,
            9 => CursorIcon::NeResize,
            6 => CursorIcon::SwResize,
            10 => CursorIcon::SeResize,
            _ if self.in_grid(y) && !self.mouse_reporting() => CursorIcon::Text,
            _ => CursorIcon::Default,
        };
        if self.cursor != Some(wanted) {
            self.cursor = Some(wanted);
            if let Some(p) = &self.themed_pointer {
                let _ = p.set_cursor(&self.conn, wanted);
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl DataDeviceHandler for App {
    /// Files or text dragged onto the window are typed in as (quoted) text
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64, _: &wl_surface::WlSurface) {
        let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().drag_offer()) else { return };
        let mimes = offer.with_mime_types(|m| m.to_vec());
        let mime = [URI_LIST].into_iter().chain(TEXT_MIMES).find(|t| mimes.iter().any(|m| m == t));
        offer.accept_mime_type(offer.serial, mime.map(str::to_string));
        offer.set_actions(DndAction::Copy, DndAction::Copy);
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().drag_offer()) else { return };
        let mimes = offer.with_mime_types(|m| m.to_vec());
        let files = mimes.iter().any(|m| m == URI_LIST);
        let Some(mime) = [URI_LIST].into_iter().chain(TEXT_MIMES).find(|t| mimes.iter().any(|m| m == t)) else { return };
        if let Ok(pipe) = offer.receive(mime.to_string()) {
            read_paste(pipe.into(), files, self.tx.clone());
        }
        offer.finish();
        self.drop_offer = Some(offer);
    }
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for App {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

impl DataSourceHandler for App {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource, _: String, pipe: WritePipe) {
        let Some((_, text)) = self.copy_source.as_ref().filter(|(s, _)| s.inner() == source) else { return };
        let text = text.clone();
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = std::fs::File::from(std::os::fd::OwnedFd::from(pipe)).write_all(text.as_bytes());
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

impl Dispatch<WpFractionalScaleV1, ()> for App {
    fn event(app: &mut Self, _: &WpFractionalScaleV1, event: wp_fractional_scale_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let scale = scale as f64 / 120.0;
            if app.fractional != Some(scale) {
                app.fractional = Some(scale);
                app.relayout();
            }
        }
    }
}

macro_rules! no_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for App {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
no_events!(WpFractionalScaleManagerV1, WpViewporter, WpViewport, XdgSystemBellV1);

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_keyboard!(App);
delegate_pointer!(App);
delegate_xdg_shell!(App);
delegate_xdg_window!(App);
delegate_data_device!(App);
delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_and_line_selection_snap() {
        let mut term = Term::new(20, 2);
        Parser::new().advance(&mut term, b"ls /usr/bin -la\r\nnext");
        let sel = Selection { anchor: (0, 5), head: (0, 5), mode: SelMode::Word };
        let r = sel_range(&term, sel);
        assert_eq!(term.text_between(r.start, r.end), "/usr/bin");
        let sel = Selection { anchor: (1, 2), head: (0, 1), mode: SelMode::Line };
        let r = sel_range(&term, sel);
        assert_eq!(term.text_between(r.start, r.end), "ls /usr/bin -la\nnext");
        assert_eq!(shell_quote("/home/u/My file.txt"), "'/home/u/My file.txt'");
        assert_eq!(shell_quote("/tmp/a.txt"), "/tmp/a.txt");
        assert_eq!(latin_letter(Keysym::Cyrillic_es), Some('c'));
    }
}
