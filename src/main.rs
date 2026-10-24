mod animation;
mod appmenu;
mod apps;
mod backend;
mod cursor;
mod desktop;
mod tray;
mod notifications;
mod xwayland;
mod debug_checks;
use feather_ui::cpu_profile;
mod drm_backend;
mod handlers;
mod icons;
mod input;
mod launchpad;
mod render;
mod raster;
mod rounded;
mod screenshot;
mod settings;
mod system_settings;
mod system_preferences;
mod wallpaper;
mod file_portal;
mod polkit_agent;
mod auth_dialog;
mod session_lock;
mod lock_auth;
mod power;
mod trash;
mod labwc;
mod shell;
use feather_ui::text;
mod theme;
mod ui_raster;
mod window;
mod controls;
mod control_panel;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        drm::DrmNode,
        egl::EGLDevice,
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer, ImportDma},
        winit::{self, WinitEvent},
    },
    desktop::PopupManager,
    input::{
        keyboard::{KeyboardHandle, XkbConfig},
        pointer::{CursorIcon, CursorImageStatus, PointerHandle},
        Seat, SeatState,
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale as OutputScale, Subpixel},
    reexports::{
        calloop::{
            generic::Generic,
            timer::{TimeoutAction, Timer},
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction,
        },
        wayland_server::{Display, DisplayHandle},
    },
    utils::{Logical, Physical, Point, Size, Transform, SERIAL_COUNTER},
    wayland::{
        compositor::{with_states, CompositorState},
        cursor_shape::CursorShapeManagerState,
        dmabuf::{DmabufFeedbackBuilder, DmabufGlobal, DmabufState},
        fractional_scale::{self, FractionalScaleManagerState},
        output::OutputManagerState,
        selection::{data_device::DataDeviceState, primary_selection::PrimarySelectionState},
        shell::xdg::{decoration::XdgDecorationState, XdgShellState},
        shm::ShmState,
        socket::ListeningSocketSource,
        viewporter::ViewporterState,
        xdg_activation::XdgActivationState,
    },
};

use crate::{
    apps::{spawn_session_services, spawn_wayland_command, AppAction, AppRegistry, BuiltinWindowKind},
    handlers::ClientState,
    shell::{
        AppIcon, AppKey, DockItem, DockMenuAction, DockTarget, Ghost, MacShell, MenuContext,
        ShellClickAction, SwitcherItem, R,
    },
    text::FontSystem,
    theme::{appearance, set_appearance, set_ui_scale, Appearance, WallpaperTheme, DOCK_RESERVED_HEIGHT, TOP_BAR_HEIGHT},
    window::{Edges, ManagedWindow, WindowBackend},
};

/// Where a dragged app icon comes from
#[derive(Debug, Clone)]
pub enum DragSource {
    /// A registry app shown in the Dock (pinned or running)
    DockApp { id: String, pinned: bool },
    /// A running app without .desktop entry or a minimized window: click only
    DockOther,
    Launchpad { app: usize },
}

#[derive(Debug, Clone)]
pub struct IconDrag {
    pub source: DragSource,
    pub start: (f64, f64),
    pub active: bool,
    pub ghost: Ghost,
    /// What a plain click (no drag) does on release
    pub click: ShellClickAction,
}

#[derive(Debug, Clone, Copy)]
pub enum DragOperation {
    MoveWindow {
        win_id: u64,
        start_cursor: (f64, f64),
        start_pos: (i32, i32),
    },
    ResizeWindow {
        win_id: u64,
        start_cursor: (f64, f64),
        start_rect: (i32, i32, i32, i32),
        edges: Edges,
    },
}

pub struct FeatherCompositor {
    /// Deterministic nested checks accept only their internal pointer events.
    debug_input_only: bool,
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, FeatherCompositor>,
    pub loop_signal: LoopSignal,
    pub backend: crate::backend::Backend,
    /// Cursor drawn by us on the DRM backend (the host draws it when nested)
    pub cursor: crate::cursor::CursorManager,
    pub refresh_mhz: i32,
    /// GPU shader for rounded window corners (None if it failed to compile)
    pub rounded_program: Option<smithay::backend::renderer::gles::GlesTexProgram>,
    pub damage_tracker: OutputDamageTracker,
    pub needs_redraw: bool,
    pub animations: crate::animation::Animations,
    pub appmenus: crate::appmenu::AppMenus,
    pub tray: Option<crate::tray::Tray>,
    pub notifications: Option<crate::notifications::Notifications>,
    pub xwayland: Option<crate::xwayland::Xwayland>,
    pub controls: Option<crate::controls::Controls>,
    controls_password_focus: bool,
    pub auth: crate::auth_dialog::AuthUi,
    pub polkit: Option<crate::polkit_agent::Agent>,
    pub lock: crate::session_lock::Lock,
    pub power: crate::power::Power,
    pub idle_notifier: smithay::wayland::idle_notify::IdleNotifierState<Self>,
    pub session_lock_state: smithay::wayland::session_lock::SessionLockManagerState,
    pub trash_full: bool,
    pub pointer_config: crate::settings::Pointer,
    pub show_about: bool,
    /// Pointer settings reach the hardware (DRM, or our labwc kiosk host)
    pub pointer_managed: bool,
    /// libinput devices (DRM session), to re-apply pointer settings
    pub input_devices: Vec<smithay::reexports::input::Device>,
    trash_watcher: Option<feather_ui::fileops::Watcher>,
    trash_tx: Option<smithay::reexports::calloop::channel::Sender<crate::trash::Msg>>,
    pub pending_autostart: Option<String>,
    pub idle_frame_timer_armed: bool,
    pub frame_pending: bool,
    pub frame_seq: u64,
    pub swap_interval_disabled: bool,
    pub start_time: Instant,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub xdg_activation_state: XdgActivationState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub output_manager_state: OutputManagerState,
    pub viewporter_state: ViewporterState,
    pub fractional_scale_state: FractionalScaleManagerState,
    pub cursor_shape_state: CursorShapeManagerState,
    pub popups: PopupManager,

    pub seat: Seat<Self>,
    pub keyboard: KeyboardHandle<Self>,
    pub pointer: PointerHandle<Self>,
    pub output: Output,

    pub phys_w: i32,
    pub phys_h: i32,
    pub scale_factor: f64,
    pub explicit_scale: Option<f64>,

    pub wayland_socket_name: String,
    pub fonts: FontSystem,
    pub app_registry: AppRegistry,
    pub shell: MacShell,

    pub windows: Vec<ManagedWindow>,
    pub next_win_id: u64,
    pub pointer_location: Point<f64, Logical>,
    pub active_drag: Option<DragOperation>,
    pub last_titlebar_click: Option<(Instant, u64)>,
    /// Buttons currently held (by evdev code), whether or not a client saw the press
    pub pressed_buttons: Vec<u32>,
    /// Stacking or geometry changed under a still pointer: re-send pointer focus
    pub pointer_focus_dirty: bool,
    pub frame_stats: Option<crate::render::FrameStats>,
    /// FEATHER_DEBUG_SCREENSHOT: write the next full frame to this PNG (visual tests)
    pub screenshot_request: Option<std::path::PathBuf>,
    /// Shift+Print region selection in progress
    pub screenshot_select: Option<crate::screenshot::Selection>,
    /// Desktop folder listings come back here
    pub desktop_tx: Option<smithay::reexports::calloop::channel::Sender<crate::desktop::Msg>>,
    /// Topmost window when the desktop took focus: raising another one ends desktop focus
    pub desktop_focus_top: Option<u64>,
    /// MIME types of a program's clipboard selection (empty when the compositor owns it)
    pub client_clipboard_mimes: Vec<String>,
    /// Finished screenshots (encoded off the event loop) come back here
    pub screenshot_tx: smithay::reexports::calloop::channel::Sender<Option<crate::screenshot::Finished>>,
    /// Presses handled by the compositor whose release must not reach clients
    pub consumed_buttons: Vec<u32>,
    pub compositor_cursor: Option<CursorIcon>,
    pub user_interacted: bool,
    /// Windows were added/removed/restacked: rebuild the Dock model on the next frame
    pub dock_model_dirty: bool,
    /// Pinned Dock apps (registry ids), in Dock order
    pub dock_pins: Vec<String>,
    /// Prevent dragging apps into, out of, or within the Dock (menu edits still work).
    pub dock_locked: bool,
    /// App icon being pressed/dragged from the Dock or Launchpad
    pub icon_drag: Option<IconDrag>,

    pub active_layout_name: String,
    pub active_layout_index: usize,
    pub keyboard_config: crate::settings::Keyboard,
    pub preferences: Option<crate::settings::Worker>,
    pub wallpaper_picker: Option<crate::wallpaper::Picker>,
    pub system_settings: crate::system_settings::Panel,
    pub system_preferences: crate::system_preferences::State,
    pub system_worker: Option<crate::system_preferences::Worker>,
    pub display_modes: Vec<crate::system_settings::Mode>,
    pub display_name: String,
    pub pending_display: Option<(usize,u64,f64)>,
    pub display_generation: u64,
    pub display_preference: Option<crate::settings::Display>,
    pub notes_buffer: String,
}

impl FeatherCompositor {
    /// Single source of truth for stacking → activation → keyboard focus.
    /// The topmost visible window is active; only Wayland windows receive keyboard focus.
    pub fn refresh_focus(&mut self) {
        let top = self.windows.iter().rposition(|w| w.is_visible());
        // The desktop keeps the keyboard until another window comes to the front
        if self.shell.desktop.focused && top.map(|i| self.windows[i].id) != self.desktop_focus_top {
            self.shell.desktop.focused = false;
        }
        let desktop = self.shell.desktop.focused;
        for (i, w) in self.windows.iter_mut().enumerate() {
            w.set_activated(!self.lock.active && !desktop && Some(i) == top);
        }
        let password = self.auth.is_open() || (self.shell.control_center_open && self.shell.cc.page==crate::control_panel::Page::Password);
        let target = if self.lock.active {self.lock.focus()} else if password || desktop {None} else {top.and_then(|i| self.windows[i].wl_surface().cloned())};
        let keyboard = self.keyboard.clone();
        if keyboard.current_focus() != target {
            keyboard.set_focus(self, target, SERIAL_COUNTER.next_serial());
        }
        self.shell.topbar_dirty = true;
        self.pointer_focus_dirty = true;
        self.dock_model_dirty = true;
        self.needs_redraw = true;
        self.sync_appmenu_focus();
    }

    pub fn mark_builtin_windows_dirty(&mut self) {
        for w in &mut self.windows {
            if w.is_builtin() {
                w.dirty = true;
            }
        }
    }

    pub fn apply_cursor(&mut self, image: CursorImageStatus) {
        let window = match &self.backend {
            crate::backend::Backend::Winit(b) => b.window(),
            crate::backend::Backend::Drm(_) => {
                self.cursor.status = image;
                self.needs_redraw = true;
                return;
            }
        };
        match image {
            CursorImageStatus::Hidden => window.set_cursor_visible(false),
            CursorImageStatus::Named(icon) => {
                window.set_cursor_visible(true);
                window.set_cursor(icon);
            }
            CursorImageStatus::Surface(_) => {
                window.set_cursor_visible(true);
                window.set_cursor(CursorIcon::Default);
            }
        }
    }

    pub fn apply_hidpi_scale(&mut self, new_scale: f64) {
        let clamped = new_scale.clamp(1.0, 2.5);
        self.scale_factor = clamped;
        set_ui_scale(clamped as f32);

        let logical_w = ((self.phys_w as f64) / clamped).round().max(640.0) as i32;
        let logical_h = ((self.phys_h as f64) / clamped).round().max(480.0) as i32;

        self.output.change_current_state(
            Some(OutputMode {
                size: (self.phys_w, self.phys_h).into(),
                refresh: self.refresh_mhz,
            }),
            Some(Transform::Normal),
            Some(OutputScale::Fractional(clamped)),
            Some((0, 0).into()),
        );

        self.shell.reallocate_buffers(logical_w, logical_h);
        self.cursor.clear_cache();

        let (sw, sh) = (self.shell.screen_w, self.shell.screen_h);
        for win in &mut self.windows {
            win.dirty = true;
            if let WindowBackend::Wayland(ref toplevel) = win.backend {
                with_states(toplevel.wl_surface(), |states| {
                    fractional_scale::with_fractional_scale(states, |fs| {
                        fs.set_preferred_scale(clamped);
                    });
                });
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_configure();
                }
            }
            // Re-fit maximized/fullscreen windows to the new logical screen size
            if win.fullscreen {
                win.resize_client(sw, sh);
            } else if win.maximized {
                win.resize_client(sw, sh - TOP_BAR_HEIGHT - DOCK_RESERVED_HEIGHT - win.titlebar_h());
            } else {
                if win.is_builtin(){win.resize_client(win.width.min(sw-24),win.height.min(sh-TOP_BAR_HEIGHT-48));}
                win.x=win.x.clamp(0,(sw-win.width).max(0));
                win.y=win.y.clamp(TOP_BAR_HEIGHT,(sh-win.titlebar_h()-48).max(TOP_BAR_HEIGHT));
            }
        }
        self.needs_redraw = true;
    }

    pub fn open_or_focus_builtin(&mut self, kind: BuiltinWindowKind) {
        if let Some(pos) = self
            .windows
            .iter()
            .position(|w| matches!(w.backend, WindowBackend::Builtin(k) if k == kind))
        {
            let mut win = self.windows.remove(pos);
            win.minimized = false;
            win.dirty = true;
            self.windows.push(win);
        } else {
            let id = self.next_win_id;
            self.next_win_id += 1;
            let offset = ((self.windows.len() as i32) % 5) * 30;
            let x = (self.shell.screen_w / 2 - 260 + offset).max(32);
            let y = (TOP_BAR_HEIGHT + 54 + offset).max(TOP_BAR_HEIGHT + 12);
            self.windows.push(ManagedWindow::new_builtin(id, kind, x, y));
        }
        self.shell.dock_dirty = true;
        if let Some(win)=self.windows.last_mut(){
            win.resize_client(win.width.min(self.shell.screen_w-32),win.height.min(self.shell.screen_h-TOP_BAR_HEIGHT-48));
            win.x=win.x.clamp(0,(self.shell.screen_w-win.width).max(0));
        }
        self.refresh_focus();
    }

    /// Name shown in bold in the menu bar: the frontmost app, like macOS
    pub fn active_window_title(&self) -> String {
        match self.windows.iter().rev().find(|w| w.is_visible()) {
            Some(w) => match self.app_key(w) {
                AppKey::App(i) => self.app_registry.apps[i].name.clone(),
                AppKey::Window(_) => w.title.clone(),
            },
            None => "Пёрышко".to_string(),
        }
    }

    /// Which app a window belongs to (unknown apps are grouped by app_id)
    pub fn app_key(&self, w: &ManagedWindow) -> AppKey {
        if let Some(i) = self.app_registry.app_for_app_id(&w.app_id) {
            return AppKey::App(i);
        }
        let first = self
            .windows
            .iter()
            .filter(|o| o.mapped && !w.app_id.is_empty() && o.app_id == w.app_id)
            .map(|o| o.id)
            .min()
            .unwrap_or(w.id);
        AppKey::Window(first)
    }

    fn icon_for_key(&self, key: AppKey, w: Option<&ManagedWindow>) -> (String, AppIcon) {
        match key {
            AppKey::App(i) => {
                let app = &self.app_registry.apps[i];
                (app.name.clone(), AppIcon::of(app))
            }
            AppKey::Window(_) => {
                let w = w.expect("window for unknown app");
                (
                    w.title.clone(),
                    AppIcon {
                        kind: crate::apps::IconKind::GenericApp,
                        name: (!w.app_id.is_empty()).then(|| w.app_id.clone()),
                    },
                )
            }
        }
    }

    /// Dock model: pinned apps, then running unpinned apps, then minimized windows
    pub fn build_dock_items(&self) -> Vec<DockItem> {
        let mut items = Vec::new();
        let keys: Vec<(AppKey, &ManagedWindow)> = self
            .windows
            .iter()
            .filter(|w| w.mapped)
            .map(|w| (self.app_key(w), w))
            .collect();

        // The pinned app currently being dragged out of the Dock leaves its slot
        let dragged = match &self.icon_drag {
            Some(IconDrag {
                source: DragSource::DockApp { id, pinned: true },
                active: true,
                ..
            }) => Some(id.as_str()),
            _ => None,
        };
        let pinned: Vec<usize> = self.pinned_indices();
        for &i in &pinned {
            let app = &self.app_registry.apps[i];
            if Some(app.id.as_str()) == dragged {
                continue;
            }
            items.push(DockItem {
                target: DockTarget::App(i),
                label: app.name.clone(),
                icon: AppIcon::of(app),
                windows: keys.iter().filter(|(k, _)| *k == AppKey::App(i)).count().min(255) as u8,
                pinned: true,
            });
        }

        let mut by_launch: Vec<&(AppKey, &ManagedWindow)> = keys.iter().collect();
        by_launch.sort_by_key(|(_, w)| w.id);
        let mut seen: Vec<AppKey> = Vec::new();
        for (key, w) in by_launch {
            if seen.contains(key) {
                continue;
            }
            seen.push(*key);
            if let AppKey::App(i) = key {
                if pinned.contains(i) {
                    continue;
                }
            }
            let (label, icon) = self.icon_for_key(*key, Some(w));
            let target = match key {
                AppKey::App(i) => DockTarget::App(*i),
                AppKey::Window(id) => DockTarget::Window(*id),
            };
            items.push(DockItem {
                target,
                label,
                icon,
                windows: keys.iter().filter(|(k, _)| k == key).count().min(255) as u8,
                pinned: false,
            });
        }

        for (key, w) in keys.iter().filter(|(_, w)| w.minimized) {
            let (_, icon) = self.icon_for_key(*key, Some(w));
            items.push(DockItem {
                target: DockTarget::Minimized(w.id),
                label: w.title.clone(),
                icon,
                windows: 0,
                pinned: false,
            });
        }
        items.push(DockItem {
            target: DockTarget::Trash,
            label: "Корзина".into(),
            icon: AppIcon {
                kind: if self.trash_full { crate::apps::IconKind::TrashFull } else { crate::apps::IconKind::TrashEmpty },
                name: None,
            },
            windows: 0,
            pinned: false,
        });
        items
    }

    /// Apps in most-recently-used order for Alt+Tab
    pub fn switcher_items(&self) -> Vec<SwitcherItem> {
        let mut items: Vec<SwitcherItem> = Vec::new();
        // Visible windows first (top of the stack = most recent), then minimized ones
        let order = self
            .windows
            .iter()
            .rev()
            .filter(|w| w.is_visible())
            .chain(self.windows.iter().rev().filter(|w| w.mapped && w.minimized));
        for w in order {
            let key = self.app_key(w);
            if items.iter().any(|i| i.key == key) {
                continue;
            }
            let (label, icon) = self.icon_for_key(key, Some(w));
            items.push(SwitcherItem { key, label, icon });
        }
        items
    }

    /// App of the frontmost visible window
    pub fn focused_app(&self) -> Option<AppKey> {
        self.windows
            .iter()
            .rev()
            .find(|w| w.is_visible())
            .map(|w| self.app_key(w))
    }

    /// Minimizes ("hides") every window whose app matches `pred`
    fn set_app_minimized(&mut self, pred: impl Fn(AppKey) -> bool) {
        let keys: Vec<(u64, AppKey)> = self
            .windows
            .iter()
            .filter(|w| w.mapped)
            .map(|w| (w.id, self.app_key(w)))
            .collect();
        for (id, key) in keys {
            if pred(key) {
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                    w.minimized = true;
                }
            }
        }
        self.refresh_focus();
    }

    /// Snapshot of the frontmost app for the App / Window menus
    pub fn build_menu_context(&self) -> MenuContext {
        let Some(top) = self.windows.iter().rev().find(|w| w.is_visible()) else {
            return MenuContext::default();
        };
        let key = self.app_key(top);
        let app_name = self.active_window_title();
        let windows = self
            .windows
            .iter()
            .filter(|w| w.mapped && self.app_key(w) == key)
            .map(|w| (w.id, w.title.clone(), w.id == top.id, w.minimized))
            .collect();
        let pinned = match key {
            AppKey::App(i) if self.app_registry.apps[i].id == crate::apps::AppRegistry::PERMANENT => None,
            AppKey::App(i) => Some(self.is_pinned(i)),
            AppKey::Window(_) => None,
        };
        MenuContext {
            app_name,
            windows,
            focused_is_wayland: !top.is_builtin(),
            pinned,
        }
    }

    /// Brings all windows of an app to the front (macOS semantics); launches it if not running
    pub fn activate_app(&mut self, key: AppKey) {
        let ids: Vec<u64> = self
            .windows
            .iter()
            .filter(|w| w.mapped && self.app_key(w) == key)
            .map(|w| w.id)
            .collect();
        if ids.is_empty() {
            if let AppKey::App(i) = key {
                self.launch_app_instance(i);
            }
            return;
        }
        let all_minimized = ids
            .iter()
            .all(|id| self.windows.iter().any(|w| w.id == *id && w.minimized));
        if all_minimized {
            // Restore only the most recent one, like clicking a Dock icon on macOS
            if let Some(&id) = ids.last() {
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                    w.minimized = false;
                }
            }
        }
        // Raise visible windows of the app, keeping their relative order
        let (mut app_wins, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.windows)
            .into_iter()
            .partition(|w| ids.contains(&w.id) && !w.minimized);
        self.windows = rest;
        self.windows.append(&mut app_wins);
        self.refresh_focus();
    }

    pub fn toggle_appearance(&mut self) {
        let next = match appearance() {
            Appearance::Light => Appearance::Dark,
            Appearance::Dark => Appearance::Light,
        };
        set_appearance(next);
        self.shell.theme = WallpaperTheme::for_appearance(next);
        self.shell.invalidate_all();
        for w in &mut self.windows {
            w.dirty = true;
        }
        self.dock_model_dirty = true;
        self.save_settings();
    }

    pub fn save_settings(&self) {
        let settings=crate::settings::Settings {
            show_about: self.show_about,
            power: self.power.preferences,
            pointer: self.pointer_config.clone(),
            appearance: appearance(),
            wallpaper: self.shell.theme,
            wallpaper_image: self.shell.wallpaper_image.clone(),
            default_browser: self.system_preferences.browser.clone(),
            default_terminal: Some(self.system_preferences.terminal.clone()),
            scale: self.explicit_scale,
            dock_zoom: self.shell.dock_zoom_enabled,
            dock_locked: self.dock_locked,
            do_not_disturb: self.shell.dnd_enabled,
            dnd_schedule: self.shell.dnd_schedule,
            notify_apps: self.shell.notify_apps.clone(),
            notify_sounds: self.shell.notify_sounds,
            dock: Some(self.dock_pins.clone()),
            animations: self.animations.enabled(),
            keyboard: self.keyboard_config.clone(),
            display: self.display_preference.clone(),
        };
        if let Some(worker)=&self.preferences{worker.save(settings);}
    }

    /// Registry indices of pinned apps, in Dock order (unknown ids are skipped)
    pub fn pinned_indices(&self) -> Vec<usize> {
        let permanent = self.app_registry.index_of(crate::apps::AppRegistry::PERMANENT);
        // Old configs may omit Files; otherwise its saved position is authoritative.
        permanent
            .filter(|_| !self.dock_pins.iter().any(|id| id == crate::apps::AppRegistry::PERMANENT))
            .into_iter()
            .chain(
                self.dock_pins
                    .iter()
                    .filter_map(|id| self.app_registry.index_of(id)),
            )
            .collect()
    }

    pub fn is_pinned(&self, app: usize) -> bool {
        let id = &self.app_registry.apps[app].id;
        id == crate::apps::AppRegistry::PERMANENT || self.dock_pins.iter().any(|p| p == id)
    }

    pub fn pin_app(&mut self, id: &str, at: Option<usize>) {
        self.dock_pins.retain(|p| p != id);
        let at = at.unwrap_or(self.dock_pins.len()).min(self.dock_pins.len());
        self.dock_pins.insert(at, id.to_string());
        self.dock_model_dirty = true;
        self.save_settings();
    }

    pub fn unpin_app(&mut self, id: &str) {
        if id == crate::apps::AppRegistry::PERMANENT {
            return;
        }
        self.dock_pins.retain(|p| p != id);
        self.dock_model_dirty = true;
        self.save_settings();
    }

    // ───────── Dragging apps between Launchpad and the Dock ─────────

    pub fn begin_icon_press(&mut self, source: DragSource, icon: AppIcon, label: String, click: ShellClickAction) {
        let pos = self.pointer_location;
        self.icon_drag = Some(IconDrag {
            source,
            start: (pos.x, pos.y),
            active: false,
            ghost: Ghost { icon, label },
            click,
        });
    }

    /// Pointer moved while an icon is pressed: start/continue dragging
    pub fn update_icon_drag(&mut self) {
        let pos = self.pointer_location;
        let Some(drag) = self.icon_drag.as_mut() else { return };
        if matches!(drag.source, DragSource::DockOther) {
            return;
        }
        // Keep normal click handling on release, without a ghost or a drop target.
        if self.dock_locked {
            return;
        }
        if !drag.active {
            let moved = ((pos.x - drag.start.0).powi(2) + (pos.y - drag.start.1).powi(2)).sqrt();
            if moved < 6.0 {
                return;
            }
            drag.active = true;
            let ghost = drag.ghost.clone();
            let from_launchpad = matches!(drag.source, DragSource::Launchpad { .. });
            self.shell.set_ghost(Some(ghost));
            self.shell.dock_dragging = true;
            self.shell.close_menus();
            self.shell.update_pointer_hover(-1000.0, -1000.0);
            if from_launchpad {
                // Reveal the Dock to drop onto, like dragging out of Launchpad on macOS
                self.shell.launchpad = None;
            }
            self.dock_model_dirty = true;
        }
        let can_pin = !matches!(drag.source, DragSource::DockApp { pinned: false, .. })
            || self.shell.dock_in_pinned_section(pos.x);
        let gap = (can_pin && self.shell.dock_in_drop_zone(pos.x, pos.y))
            .then(|| self.shell.dock_insert_index(pos.x));
        self.shell.set_dock_gap(gap);
        self.needs_redraw = true;
    }

    /// Button released: a click if the icon never moved, otherwise a drop
    pub fn finish_icon_press(&mut self) {
        let Some(drag) = self.icon_drag.take() else { return };
        self.shell.set_ghost(None);
        self.shell.dock_dragging = false;
        let gap = self.shell.dock_gap.take();
        self.shell.dock_dirty = true;
        self.dock_model_dirty = true;
        self.needs_redraw = true;

        // A blocked drag must not turn into a launch/activation at its release point.
        let pos = self.pointer_location;
        if self.dock_locked && ((pos.x - drag.start.0).hypot(pos.y - drag.start.1) >= 6.0 || drag.active) {
            return;
        }
        if !drag.active {
            self.execute_shell_action(drag.click);
            return;
        }
        let in_dock = self.shell.dock_in_drop_zone(pos.x, pos.y);
        match drag.source {
            DragSource::DockApp { id, pinned } => {
                if in_dock && gap.is_some() {
                    self.pin_app(&id, gap);
                } else if pinned && id != crate::apps::AppRegistry::PERMANENT {
                    self.unpin_app(&id);
                    self.shell.notify("Удалено из Dock", drag.ghost.label);
                }
            }
            DragSource::Launchpad { app } => {
                if in_dock {
                    let id = self.app_registry.apps[app].id.clone();
                    self.pin_app(&id, gap);
                }
            }
            DragSource::DockOther => {}
        }
    }

    /// Fast launches stay still; only a launch without a window after 200 ms bounces.
    pub fn launch_app_instance(&mut self, i: usize) {
        let app = &self.app_registry.apps[i];
        let action = ShellClickAction::LaunchApp(app.action.clone(), app.name.clone());
        let builtin = app.is_builtin();
        self.execute_shell_action(action);
        if !builtin && self.animations.enabled() {
            let first = self.next_win_id;
            let launch = Instant::now();
            self.shell.pending_bounce = Some(launch);
            if self.shell.bounce.take().is_some() {
                self.shell.dock_dirty = true;
                self.needs_redraw = true;
            }
            let _ = self.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(200)), move |_, _, state| {
                if state.shell.pending_bounce == Some(launch) {
                    state.shell.pending_bounce = None;
                    let appeared = state.windows.iter().any(|w| w.mapped && w.id >= first && state.app_key(w) == AppKey::App(i));
                    if !appeared && state.animations.enabled() {
                        state.shell.start_bounce(i, first, &mut state.fonts);
                        state.needs_redraw = true;
                    }
                }
                TimeoutAction::Drop
            });
        }
    }

    /// Stops the bounce once the launched app has a window (after the current jump)
    pub fn advance_bounce(&mut self) {
        let Some(b) = self.shell.bounce.as_ref() else { return };
        let (app, first) = (b.app, b.first_new_window);
        let now = Instant::now();
        if self.windows.iter().any(|w| w.mapped && w.id >= first && self.app_key(w) == AppKey::App(app)) {
            if let Some(b) = self.shell.bounce.as_mut() {
                b.finish(now);
            }
        }
        let over = self.shell.bounce.as_ref().is_some_and(|b| b.offset(now).1);
        if over {
            self.shell.bounce = None;
            self.shell.dock_dirty = true;
        }
        self.needs_redraw = true;
    }

    /// Right click on a Dock icon: the app's windows, a new window, pin, quit
    pub fn open_dock_menu_for(&mut self, target: DockTarget, anchor: R) {
        if target == DockTarget::Trash {
            let mut items = vec![("Открыть".to_string(), DockMenuAction::Open)];
            if self.trash_full {
                items.push(("Очистить Корзину…".to_string(), DockMenuAction::EmptyTrash));
            }
            items.push(self.dock_lock_menu_item());
            self.shell.open_dock_menu(target, anchor, items, &mut self.fonts);
            return;
        }
        let key = self.dock_target_key(target);
        let running = match target {
            DockTarget::App(i) => self.windows.iter().any(|w| w.mapped && self.app_key(w) == AppKey::App(i)),
            DockTarget::Window(_) | DockTarget::Minimized(_) => true,
            DockTarget::Trash => false,
        };
        let mut items = Vec::new();
        if let (true, Some(key)) = (running, key) {
            let front = self.windows.iter().rev().find(|w| w.is_visible()).map(|w| w.id);
            for w in self.windows.iter().filter(|w| w.mapped && self.app_key(w) == key) {
                let mark = if Some(w.id) == front { "✓ " } else if w.minimized { "◇ " } else { "    " };
                let title = if w.title.is_empty() { "Без названия".to_string() } else { w.title.chars().take(48).collect() };
                items.push((format!("{mark}{title}"), DockMenuAction::Window(w.id)));
            }
        } else {
            items.push(("Открыть".to_string(), DockMenuAction::Open));
        }
        if let DockTarget::App(i) = target {
            if running && !self.app_registry.apps[i].is_builtin() {
                items.push(("Новое окно".to_string(), DockMenuAction::NewWindow));
            }
        }
        if let DockTarget::App(i) = target {
            if self.app_registry.apps[i].id == crate::apps::AppRegistry::PERMANENT {
                // Files stays in the Dock, as Finder does
            } else if self.is_pinned(i) {
                items.push(("Открепить от Dock".to_string(), DockMenuAction::RemoveFromDock));
            } else {
                items.push(("Закрепить в Dock".to_string(), DockMenuAction::KeepInDock));
            }
        }
        if running {
            items.push(("Завершить".to_string(), DockMenuAction::Quit));
        }
        items.push(self.dock_lock_menu_item());
        self.shell.open_dock_menu(target, anchor, items, &mut self.fonts);
    }

    fn dock_lock_menu_item(&self) -> (String, DockMenuAction) {
        (
            if self.dock_locked { "✓ Заблокировать Dock" } else { "Заблокировать Dock" }.into(),
            DockMenuAction::ToggleDockLock,
        )
    }

    pub fn open_dock_background_menu(&mut self) {
        self.shell.open_context_menu(
            vec![self.dock_lock_menu_item()],
            self.pointer_location.x as f32,
            self.pointer_location.y as f32,
            &mut self.fonts,
        );
    }

    fn dock_target_key(&self, target: DockTarget) -> Option<AppKey> {
        match target {
            DockTarget::App(i) => Some(AppKey::App(i)),
            DockTarget::Window(id) | DockTarget::Minimized(id) => {
                self.windows.iter().find(|w| w.id == id).map(|w| self.app_key(w))
            }
            DockTarget::Trash => None,
        }
    }

    /// Asks every window of an app to close (builtin windows close immediately)
    pub fn quit_app(&mut self, key: AppKey) {
        let ids: Vec<u64> = self
            .windows
            .iter()
            .filter(|w| w.mapped && self.app_key(w) == key)
            .map(|w| w.id)
            .collect();
        for id in ids {
            if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
                match self.windows[pos].backend {
                    WindowBackend::Wayland(ref t) => t.send_close(),
                    WindowBackend::Builtin(_) => {
                        self.windows.remove(pos);
                    }
                }
            }
        }
        self.refresh_focus();
    }

    pub fn toggle_launchpad(&mut self) {
        tracing::debug!("toggle_launchpad: открыт={}", self.shell.launchpad.is_some());
        if self.shell.launchpad.take().is_none() {
            self.shell.close_menus();
            self.shell.launchpad = Some(crate::launchpad::Launchpad::new(
                &self.app_registry,
                self.shell.screen_w,
                self.shell.screen_h,
            ));
        }
        self.needs_redraw = true;
    }

    pub fn notification_update(&mut self, update: crate::notifications::Update) {
        match update {
            crate::notifications::Update::Show(n) => self.shell.show_notification(*n, &self.app_registry),
            crate::notifications::Update::Close(id) => { self.shell.close_notification(id); }
        }
        if std::mem::take(&mut self.shell.notify_apps_changed) {
            self.save_settings();
            self.mark_settings_dirty();
        }
        self.flush_notification_events();
        self.needs_redraw = true;
    }

    /// Hands banner closes/actions to the D-Bus worker; an action gets an activation
    /// token first so the app may raise its window.
    pub fn flush_notification_events(&mut self) {
        for event in std::mem::take(&mut self.shell.notification_events) {
            let Some(server) = &self.notifications else { continue };
            match event {
                crate::shell::NotificationEvent::Closed(id, reason) => server.closed(id, reason),
                crate::shell::NotificationEvent::Sound(file) => server.play(file),
                crate::shell::NotificationEvent::Invoked(id, key) => {
                    let token = self.xdg_activation_state.create_external_token(None).0.clone();
                    let value = token.as_str().to_string();
                    let _ = self.loop_handle.insert_source(Timer::from_duration(Duration::from_secs(10)), move |_, _, state| {
                        state.xdg_activation_state.remove_token(&token);
                        TimeoutAction::Drop
                    });
                    server.invoked(id, key, Some(value));
                }
            }
        }
    }

    pub fn execute_shell_action(&mut self, action: ShellClickAction) {
        self.needs_redraw = true;
        self.flush_notification_events();
        match action {
            ShellClickAction::SystemSettings(action)=>self.apply_system_setting(action),
            ShellClickAction::None => {}
            ShellClickAction::ToggleDoNotDisturb => self.apply_system_setting(crate::system_settings::Action::DoNotDisturb),
            ShellClickAction::Tray {key,action,x,y} => {
                self.shell.close_menus();
                let token=if matches!(action,crate::tray::Action::Activate|crate::tray::Action::Secondary) {
                    let token=self.xdg_activation_state.create_external_token(None).0.clone();
                    let value=token.as_str().to_string();
                    let _=self.loop_handle.insert_source(Timer::from_duration(Duration::from_secs(10)),move|_,_,state|{
                        state.xdg_activation_state.remove_token(&token);TimeoutAction::Drop
                    });
                    Some(value)
                }else{None};
                if let Some(tray)=&mut self.tray {
                    let context=matches!(action,crate::tray::Action::Context);
                    let generation=tray.interact(key.clone(),action,x,y,token);
                    if context {self.shell.prepare_tray_menu(key,generation,x as f32);}
                }
            }
            ShellClickAction::TrayMenu {generation,id} => {
                if let Some(tray)=&self.tray{tray.click(generation,id,self.start_time.elapsed().as_millis() as u32);}
            }
            ShellClickAction::TrayOverflow => {
                let items=self.shell.tray_items.iter().enumerate().map(|(index,i)|((if i.title.is_empty(){&i.key}else{&i.title}).chars().take(48).collect(),DockMenuAction::Tray(index))).collect();
                self.shell.open_context_menu(items,self.pointer_location.x as f32,TOP_BAR_HEIGHT as f32+4.0,&mut self.fonts);
            }
            ShellClickAction::LaunchpadLaunch(app) => {
                self.shell.launchpad = None;
                self.pointer_focus_dirty = true;
                self.launch_app_instance(app);
            }
            ShellClickAction::Control(command) => {
                if let Some(controls)=&self.controls {
                    if let Err(e)=controls.send(command) { self.shell.cc.error=Some(e.into()); self.shell.overlay_dirty=true; }
                }
            }
            ShellClickAction::NetworkSettings => self.execute_shell_action(ShellClickAction::LaunchApp(AppAction::Exec("nm-connection-editor".into()),"Настройки сети".into())),
            ShellClickAction::BluetoothSettings => self.execute_shell_action(ShellClickAction::LaunchApp(AppAction::Exec("blueman-manager".into()),"Bluetooth".into())),
            ShellClickAction::AppMenuActivate { generation, id } => {
                self.appmenus.click(generation, id, self.start_time.elapsed().as_millis() as u32);
            }
            ShellClickAction::LaunchApp(app_action, app_name) => match app_action {
                AppAction::Exec(cmd) => {
                    if spawn_wayland_command(&cmd, &self.wayland_socket_name) {
                        self.shell.notify(
                            format!("Запуск: {}", app_name),
                            format!("Команда `{}` запущена на {}", cmd, self.wayland_socket_name),
                        );
                    } else {
                        self.shell.notify(
                            format!("Не удалось запустить {}", app_name),
                            format!("Проверьте наличие пакета `{}` в системе", cmd),
                        );
                    }
                }
                AppAction::BuiltinWindow(kind) => self.open_or_focus_builtin(kind),
                AppAction::HotReloadShell => self.trigger_hot_reload(),
                AppAction::OpenLaunchpad => self.toggle_launchpad(),
            },
            ShellClickAction::DockPress(target) => {
                self.shell.anchor_dock_target(target);
                // Executed on release if the icon was not dragged, or as a direct click.
                // Toggle only the frontmost window; activation from Alt+Tab/menus stays explicit.
                if let Some(key) = self.dock_target_key(target).filter(|key| !matches!(target, DockTarget::Minimized(_)) && self.focused_app() == Some(*key)) {
                    // Several windows: bring the next one forward; one window: minimize it
                    let visible: Vec<usize> = (0..self.windows.len()).filter(|&i| self.windows[i].is_visible() && self.app_key(&self.windows[i]) == key).collect();
                    if visible.len() > 1 {
                        let w = self.windows.remove(visible[0]);
                        self.windows.push(w);
                    } else if let Some(win) = self.windows.iter_mut().rev().find(|w| w.is_visible()) {
                        win.minimized = true;
                    }
                    self.refresh_focus();
                    return;
                }
                let action = match target {
                    DockTarget::App(i) => ShellClickAction::ActivateApp(i),
                    DockTarget::Window(id) => ShellClickAction::ActivateWindow(id),
                    DockTarget::Minimized(id) => ShellClickAction::RestoreMinimizedWindow(id),
                    DockTarget::Trash => {
                        self.open_trash();
                        return;
                    }
                };
                self.execute_shell_action(action);
            }
            ShellClickAction::DockMenu(DockMenuAction::Desktop(action), _) => self.desktop_menu_action(action),
            ShellClickAction::DockMenu(action, target) => match action {
                DockMenuAction::Tray(index) => {
                    if let Some(item)=self.shell.tray_items.get(index){
                        self.execute_shell_action(ShellClickAction::Tray{key:item.key.clone(),action:crate::tray::Action::Context,x:self.pointer_location.x as i32,y:TOP_BAR_HEIGHT});
                    }
                }
                DockMenuAction::Open if target == DockTarget::Trash => self.open_trash(),
                DockMenuAction::EmptyTrash => self.confirm_empty_trash(),
                DockMenuAction::ToggleDockLock => {
                    self.dock_locked = !self.dock_locked;
                    self.save_settings();
                }
                DockMenuAction::Open => {
                    if let Some(key) = self.dock_target_key(target) {
                        self.activate_app(key);
                    }
                    if let DockTarget::Minimized(id) = target {
                        self.execute_shell_action(ShellClickAction::RestoreMinimizedWindow(id));
                    }
                }
                DockMenuAction::KeepInDock => {
                    if let DockTarget::App(i) = target {
                        let id = self.app_registry.apps[i].id.clone();
                        self.pin_app(&id, None);
                    }
                }
                DockMenuAction::RemoveFromDock => {
                    if let DockTarget::App(i) = target {
                        let id = self.app_registry.apps[i].id.clone();
                        self.unpin_app(&id);
                    }
                }
                DockMenuAction::Quit => {
                    if let Some(key) = self.dock_target_key(target) {
                        self.quit_app(key);
                    }
                }
                DockMenuAction::Window(id) => {
                    let minimized = self.windows.iter().any(|w| w.id == id && w.minimized);
                    self.execute_shell_action(if minimized { ShellClickAction::RestoreMinimizedWindow(id) } else { ShellClickAction::ActivateWindow(id) });
                }
                DockMenuAction::NewWindow => {
                    if let DockTarget::App(i) = target {
                        self.launch_app_instance(i);
                    }
                }
                DockMenuAction::Desktop(_) => {}
            },
            ShellClickAction::ActivateApp(idx) => self.activate_app(AppKey::App(idx)),
            ShellClickAction::ActivateWindow(win_id) => {
                self.execute_shell_action(ShellClickAction::FocusWindow(win_id));
            }
            ShellClickAction::ToggleAppearance => self.toggle_appearance(),
            ShellClickAction::RestoreMinimizedWindow(win_id) => {
                if let Some(pos) = self.windows.iter().position(|w| w.id == win_id) {
                    let mut win = self.windows.remove(pos);
                    win.minimized = false;
                    win.dirty = true;
                    self.windows.push(win);
                    self.shell.dock_dirty = true;
                    self.refresh_focus();
                }
            }
            ShellClickAction::ToggleLayout => {
                self.select_input_source((self.active_layout_index+1)%self.keyboard_config.layouts.len());
            }
            ShellClickAction::CycleTheme => {
                self.shell.wallpaper_image=None;
                self.shell.theme = self.shell.theme.next();
                self.shell.invalidate_all();
                self.mark_builtin_windows_dirty();
                self.save_settings();
            }
            ShellClickAction::CycleHiDpiScale => {
                let steps = [1.0, 1.25, 1.5, 1.75, 2.0];
                let cur = self.scale_factor;
                let next = steps
                    .iter()
                    .copied()
                    .find(|&s| s > cur + 0.05)
                    .unwrap_or(1.0);
                self.explicit_scale = Some(next);
                self.apply_hidpi_scale(next);
                self.save_settings();
                self.shell.notify(
                    format!("Масштаб HiDPI Retina: {}%", (next * 100.0).round() as i32),
                    "Векторный интерфейс и wp_fractional_scale_v1 обновлены на лету",
                );
            }
            ShellClickAction::ToggleDockZoom => {
                self.shell.dock_zoom_enabled = !self.shell.dock_zoom_enabled;
                self.shell.dock_dirty = true;
                self.mark_builtin_windows_dirty();
                self.save_settings();
            }
            ShellClickAction::HotReloadShell => self.trigger_hot_reload(),
            ShellClickAction::MinimizeAllWindows => {
                for w in &mut self.windows {
                    w.minimized = true;
                }
                self.shell.dock_dirty = true;
                self.refresh_focus();
            }
            ShellClickAction::QuitCompositor => self.loop_signal.stop(),
            ShellClickAction::HideApp => {
                if let Some(key) = self.focused_app() {
                    self.set_app_minimized(|k| k == key);
                }
            }
            ShellClickAction::HideOthers => {
                if let Some(key) = self.focused_app() {
                    self.set_app_minimized(|k| k != key);
                }
            }
            ShellClickAction::ShowAllWindows => {
                for w in &mut self.windows {
                    w.minimized = false;
                }
                self.refresh_focus();
            }
            ShellClickAction::QuitFocusedApp => {
                if let Some(key) = self.focused_app() {
                    self.quit_app(key);
                }
            }
            ShellClickAction::MinimizeFocused => {
                if let Some(w) = self.windows.iter_mut().rev().find(|w| w.is_visible()) {
                    w.minimized = true;
                }
                self.refresh_focus();
            }
            ShellClickAction::ZoomFocused => {
                let (sw, sh) = (self.shell.screen_w, self.shell.screen_h);
                if let Some(w) = self.windows.iter_mut().rev().find(|w| w.is_visible()) {
                    w.resize_anchor = None;
                    w.toggle_maximize(sw, sh);
                }
            }
            ShellClickAction::FullscreenFocused => {
                let (sw, sh) = (self.shell.screen_w, self.shell.screen_h);
                if let Some(w) = self.windows.iter_mut().rev().find(|w| w.is_visible()) {
                    let on = !w.fullscreen;
                    w.resize_anchor = None;
                    w.set_fullscreen(on, sw, sh);
                }
            }
            ShellClickAction::CloseFocused => {
                if let Some(idx) = self.windows.iter().rposition(|w| w.is_visible()) {
                    match self.windows[idx].backend {
                        WindowBackend::Wayland(ref t) => t.send_close(),
                        WindowBackend::Builtin(_) => {
                            self.windows.remove(idx);
                            self.refresh_focus();
                        }
                    }
                }
            }
            ShellClickAction::FocusWindow(id) => {
                if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
                    let mut w = self.windows.remove(pos);
                    w.minimized = false;
                    self.windows.push(w);
                    self.refresh_focus();
                }
            }
            ShellClickAction::ToggleShowAbout => {
                self.show_about = !self.show_about;
                self.mark_builtin_windows_dirty();
                self.needs_redraw = true;
                self.save_settings();
            }
            ShellClickAction::TogglePinFocused => {
                if let Some(AppKey::App(i)) = self.focused_app() {
                    let id = self.app_registry.apps[i].id.clone();
                    if self.is_pinned(i) {
                        self.unpin_app(&id);
                    } else {
                        self.pin_app(&id, None);
                    }
                }
            }
            ShellClickAction::ShowShortcuts => self.shell.notify(
                "Горячие клавиши",
                "Super+Space поиск • Super+A приложения • Alt+Tab • Super+Q/W/H/M/F",
            ),
        }
    }

    pub fn trigger_hot_reload(&mut self) {
        self.app_registry = AppRegistry::new();
        self.shell.reload_count += 1;
        self.shell.invalidate_all();
        self.dock_model_dirty = true;
        for w in &mut self.windows {
            w.dirty = true;
        }
        self.shell.notify(
            "Оболочка перезагружена",
            "Список приложений, иконки и оформление обновлены без закрытия окон",
        );
    }

    /// Safety net in case a client vanished without destroying its toplevel cleanly
    fn prune_dead_windows(&mut self) {
        let before = self.windows.len();
        self.windows.retain(|w| match w.backend {
            WindowBackend::Wayland(ref toplevel) => toplevel.alive(),
            WindowBackend::Builtin(_) => true,
        });
        if self.windows.len() != before {
            self.shell.topbar_dirty = true;
            self.shell.dock_dirty = true;
            self.refresh_focus();
        }
        self.popups.cleanup();
    }

    fn on_resized(&mut self, size: Size<i32, Physical>, host_scale: f64) {
        self.phys_w = size.w.max(640);
        self.phys_h = size.h.max(480);
        self.damage_tracker = OutputDamageTracker::new(size, 1.0, Transform::Flipped180);

        let scale = self
            .explicit_scale
            .unwrap_or_else(|| auto_scale_for_resolution(self.phys_w, host_scale));
        self.apply_hidpi_scale(scale);

        let (sw, sh) = (self.shell.screen_w, self.shell.screen_h);
        let recenter = !self.user_interacted;
        for (idx, win) in self.windows.iter_mut().enumerate() {
            if win.maximized || win.fullscreen {
                continue;
            }
            if recenter {
                let offset = ((idx as i32) % 5) * 28;
                win.x = ((sw - win.width) / 2 + offset).max(0);
                win.y = ((sh - win.total_height()) / 2 - 20 + offset).max(TOP_BAR_HEIGHT + 8);
            } else {
                win.x = win.x.clamp(-(win.width - 80), sw - 80);
                win.y = win.y.clamp(TOP_BAR_HEIGHT, sh - 40);
            }
        }
    }

    pub fn handle_winit_event(&mut self, event: WinitEvent) {
        match event {
            WinitEvent::Resized { size, scale_factor } => {self.on_resized(size, scale_factor);self.lock_resized();},
            WinitEvent::Input(event) if !self.debug_input_only => self.handle_input(event),
            WinitEvent::Input(_) => {},
            // Rendering is driven by our own damage; the host's redraw event only tells us
            // that the previous frame was presented and the next one may go out
            WinitEvent::Redraw => self.on_host_frame(),
            WinitEvent::CloseRequested => self.loop_signal.stop(),
            WinitEvent::Focus(false) if !self.debug_input_only => self.release_all_input(),
            WinitEvent::Focus(_) => {}
        }
    }
}

fn parse_explicit_scale() -> Option<f64> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--scale") {
        if let Some(val) = args.get(pos + 1).and_then(|s| s.parse::<f64>().ok()) {
            return Some(val.clamp(1.0, 2.5));
        }
    }
    std::env::var("FEATHER_SCALE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .map(|v| v.clamp(1.0, 2.5))
}

fn auto_scale_for_resolution(phys_w: i32, host_scale: f64) -> f64 {
    if phys_w >= 3000 || host_scale >= 1.8 {
        2.0
    } else if phys_w >= 2400 || host_scale >= 1.35 {
        1.5
    } else {
        1.0
    }
}

/// Name of the GL driver actually in use (to detect a software fallback like llvmpipe)
fn gl_renderer_name(renderer: &mut GlesRenderer) -> String {
    renderer
        .with_context(|gl| unsafe {
            let ptr = gl.GetString(smithay::backend::renderer::gles::ffi::RENDERER);
            if ptr.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(ptr as *const std::ffi::c_char)
                    .to_string_lossy()
                    .into_owned()
            }
        })
        .unwrap_or_default()
}

/// Advertises linux-dmabuf v4 with the GPU's render node as main device. Mesa EGL clients
/// (GTK4, Firefox, Qt) need this to find the GPU; without it they silently fall back to
/// software rendering, which makes every client — browsers especially — sluggish.
fn setup_dmabuf(
    mut dmabuf_state: DmabufState,
    renderer: &mut GlesRenderer,
    node_hint: Option<DrmNode>,
    dh: &DisplayHandle,
) -> (DmabufState, Option<DmabufGlobal>) {
    let gl_name = gl_renderer_name(renderer);
    let software = ["llvmpipe", "softpipe", "swrast"]
        .iter()
        .any(|sw| gl_name.to_lowercase().contains(sw));
    if software {
        // Importing client GPU buffers into a software renderer is unreliable (and crashed
        // in tests); clients will use shared memory instead.
        tracing::warn!("Программный рендер ({gl_name}): dmabuf отключён");
        return (dmabuf_state, None);
    }
    tracing::info!("GPU: {gl_name}");

    let formats = renderer.dmabuf_formats();
    let render_node = node_hint
        .or_else(|| {
            EGLDevice::device_for_display(renderer.egl_context().display())
                .ok()
                .and_then(|dev| dev.try_get_render_node().ok().flatten())
        })
        .or_else(|| {
            // Fallback for EGL stacks without EGL_EXT_device_query: first render node
            let mut nodes: Vec<_> = std::fs::read_dir("/dev/dri")
                .ok()?
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("renderD"))
                })
                .collect();
            nodes.sort();
            nodes.into_iter().find_map(|p| DrmNode::from_path(p).ok())
        });

    let feedback = render_node.and_then(|node| {
        DmabufFeedbackBuilder::new(node.dev_id(), formats.clone())
            .build()
            .ok()
    });
    let global = match feedback {
        Some(ref fb) => {
            dmabuf_state.create_global_with_default_feedback::<FeatherCompositor>(dh, fb)
        }
        None => {
            tracing::warn!("dmabuf feedback недоступен, используется dmabuf v3");
            dmabuf_state.create_global::<FeatherCompositor>(dh, formats)
        }
    };
    (dmabuf_state, Some(global))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args_os:Vec<_>=std::env::args_os().collect();
    if args_os.get(1).is_some_and(|a|a=="--file-portal"){return file_portal::run();}
    if args_os.get(1).is_some_and(|a|a=="--open-terminal"){return system_preferences::launch_terminal(std::path::Path::new(args_os.get(2).ok_or("missing directory")?));}
    let args:Vec<_>=args_os.iter().map(|a|a.to_string_lossy().into_owned()).collect();
    if args.get(1).is_some_and(|a|a=="--decode-wallpaper"){return wallpaper::helper(&args);}
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        tracing_subscriber::fmt().init();
    }

    run_compositor()
}

pub fn run_compositor() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<'static, FeatherCompositor> = EventLoop::try_new()?;
    let display: Display<FeatherCompositor> = Display::new()?;
    let dh = display.handle();

    let _idle_inhibit = smithay::wayland::idle_inhibit::IdleInhibitManagerState::new::<FeatherCompositor>(&dh);
    let compositor_state = CompositorState::new::<FeatherCompositor>(&dh);
    let xdg_shell_state = XdgShellState::new::<FeatherCompositor>(&dh);
    let xdg_decoration_state = XdgDecorationState::new::<FeatherCompositor>(&dh);
    let xdg_activation_state = XdgActivationState::new::<FeatherCompositor>(&dh);
    // RGBA byte order (tiny-skia, Feather's own clients) uploads without a channel swap
    let shm_state = ShmState::new::<FeatherCompositor>(&dh, vec![smithay::reexports::wayland_server::protocol::wl_shm::Format::Abgr8888, smithay::reexports::wayland_server::protocol::wl_shm::Format::Xbgr8888]);
    let mut seat_state = SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, "feather-seat");
    let data_device_state = DataDeviceState::new::<FeatherCompositor>(&dh);
    let primary_selection_state = PrimarySelectionState::new::<FeatherCompositor>(&dh);
    let output_manager_state = OutputManagerState::new_with_xdg_output::<FeatherCompositor>(&dh);
    let viewporter_state = ViewporterState::new::<FeatherCompositor>(&dh);
    let fractional_scale_state = FractionalScaleManagerState::new::<FeatherCompositor>(&dh);
    let cursor_shape_state = CursorShapeManagerState::new::<FeatherCompositor>(&dh);

    let args: Vec<String> = std::env::args().collect();
    let is_windowed = args.iter().any(|a| a == "--windowed");
    let is_standalone = args.iter().any(|a| a == "--standalone");

    // Directly on the GPU when started from a TTY / display manager without a host compositor
    let nested = std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    let use_drm = args.iter().any(|a| a == "--drm") || (!nested && !is_windowed);

    let saved = crate::settings::Settings::load();
    let mut winit_loop = None;
    let mut drm_info = None;
    let mut backend = if use_drm {
        let d = crate::drm_backend::init(&event_loop.handle(),saved.display.as_ref())?;
        drm_info = Some((d.size, d.refresh, d.physical_mm, d.connector_name.clone(), d.render_node));
        crate::backend::Backend::Drm(Box::new(d))
    } else {
        let window_attrs = if is_windowed {
            smithay::reexports::winit::window::Window::default_attributes()
                .with_inner_size(smithay::reexports::winit::dpi::LogicalSize::new(1440.0, 900.0))
                .with_title("Пёрышко DE (Windowed)")
                .with_visible(true)
        } else {
            smithay::reexports::winit::window::Window::default_attributes()
                .with_title("Пёрышко DE")
                .with_decorations(false)
                .with_fullscreen(Some(smithay::reexports::winit::window::Fullscreen::Borderless(None)))
                .with_visible(true)
        };
        let (b, l) = winit::init_from_attributes::<GlesRenderer>(window_attrs)?;
        winit_loop = Some(l);
        crate::backend::Backend::Winit(b)
    };

    let rounded_program = crate::rounded::compile(backend.renderer());
    let dmabuf_state = DmabufState::new();
    let node_hint = drm_info.as_ref().and_then(|i| i.4);
    let (dmabuf_state, dmabuf_global) = setup_dmabuf(dmabuf_state, backend.renderer(), node_hint, &dh);

    let (initial_size, host_scale, refresh_mhz) = match &backend {
        crate::backend::Backend::Winit(b) => (b.window_size(), b.scale_factor(), 60_000),
        crate::backend::Backend::Drm(d) => (Size::from(d.size), 1.0, d.refresh.max(1000)),
    };
    let phys_w = initial_size.w.max(640);
    let phys_h = initial_size.h.max(480);

    set_appearance(saved.appearance);
    let explicit_scale = parse_explicit_scale().or(saved.scale);
    let init_scale =
        explicit_scale.unwrap_or_else(|| auto_scale_for_resolution(phys_w, host_scale));
    set_ui_scale(init_scale as f32);
    let logical_w = ((phys_w as f64) / init_scale).round().max(640.0) as i32;
    let logical_h = ((phys_h as f64) / init_scale).round().max(480.0) as i32;

    let (output_name, physical_mm) = match &drm_info {
        Some((_, _, mm, name, _)) if mm.0 > 0 => (name.clone(), *mm),
        Some((_, _, _, name, _)) => (name.clone(), (600, 340)),
        None => ("Feather-Display-1".to_string(), (600, 340)),
    };
    let output = Output::new(
        output_name,
        PhysicalProperties {
            size: physical_mm.into(),
            subpixel: Subpixel::HorizontalRgb,
            make: "Feather DE".into(),
            model: "Display".into(),
        },
    );
    let _output_global = output.create_global::<FeatherCompositor>(&dh);
    let mode = OutputMode {
        size: (phys_w, phys_h).into(),
        refresh: refresh_mhz,
    };
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        Some(OutputScale::Fractional(init_scale)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    let socket = ListeningSocketSource::new_auto()?;
    let socket_name = socket.socket_name().to_string_lossy().into_owned();

    println!(
        "🪶 Пёрышко DE (Feather Wayland Compositor) запущен на WAYLAND_DISPLAY={} (HiDPI Scale: {:.2}x, Physical: {}x{})",
        socket_name, init_scale, phys_w, phys_h
    );

    let keyboard_layouts=saved.keyboard.layouts.join(",");
    let xkb_config = XkbConfig {
        rules: "",
        model: "pc105",
        layout: &keyboard_layouts,
        variant: "",
        options: Some(saved.keyboard.shortcut.option().to_string()),
    };
    let keyboard = seat.add_keyboard(xkb_config, 250, 35)?;
    let pointer = seat.add_pointer();

    let handle = event_loop.handle();

    // New Wayland clients
    handle.insert_source(socket, |stream, _, state: &mut FeatherCompositor| {
        if let Err(err) = state
            .display_handle
            .insert_client(stream, Arc::new(ClientState::default()))
        {
            tracing::warn!("Не удалось принять клиента: {err}");
        }
    })?;

    // Client requests are dispatched as soon as they arrive, not once per frame
    handle.insert_source(
        Generic::new(display, Interest::READ, Mode::Level),
        |_, display, state| {
            // SAFETY: the display is never dropped while the event loop runs
            let res = unsafe { display.get_mut().dispatch_clients(state) };
            if let Err(err) = res {
                tracing::warn!("Ошибка обработки клиентов: {err}");
            }
            Ok(PostAction::Continue)
        },
    )?;

    if let Some(winit_loop) = winit_loop {
        handle.insert_source(winit_loop, |event, _, state| state.handle_winit_event(event))?;
    }

    // Slow housekeeping: clock, RSS, toasts. Nothing else polls.
    handle.insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, state| {
        let dnd = state.shell.dnd_active();
        let ticked = state.shell.tick();
        // A scheduled «Не беспокоить» started or ended: the Settings switch follows
        if state.shell.dnd_active() != dnd { state.mark_settings_dirty(); }
        if state.lock.clock_stale() { state.needs_redraw = true; }
        state.flush_notification_events();
        if ticked {
            state.needs_redraw = true;
            if state.system_settings.page==crate::system_settings::Page::DateTime{state.mark_settings_dirty();}
        }
        state.prune_dead_windows();
        TimeoutAction::ToDuration(Duration::from_secs(1))
    })?;

    let (screenshot_tx, screenshot_rx) = crate::screenshot::channel_source();
    event_loop.handle().insert_source(screenshot_rx, |event, _, state| {
        if let smithay::reexports::calloop::channel::Event::Msg(finished) = event {
            state.install_screenshot(finished);
        }
    })?;

    // Only used by the nested backend; DRM has its own damage tracking per output
    let damage_tracker = OutputDamageTracker::new(initial_size, 1.0, Transform::Flipped180);

    let appmenus = crate::appmenu::AppMenus::new(
        &dh, &handle, is_standalone || use_drm || std::env::var_os("FEATHER_APPMENU_REGISTRAR").is_some(),
    )?;
    let pending_autostart = args.iter().position(|a| a == "--autostart")
        .and_then(|pos| args.get(pos + 1)).cloned();

    let mut state = FeatherCompositor {
        debug_input_only: std::env::var_os("FEATHER_DEBUG_INPUT_ONLY").is_some(),
        controls: None,
        controls_password_focus: false,
        auth: Default::default(),
        polkit: None,
        lock: Default::default(),
        power: crate::power::Power::new(saved.power),
        idle_notifier: smithay::wayland::idle_notify::IdleNotifierState::new(&dh,event_loop.handle()),
        session_lock_state: smithay::wayland::session_lock::SessionLockManagerState::new::<FeatherCompositor,_>(&dh, |_|true),
        trash_full: false,
        trash_watcher: None,
        trash_tx: None,
        display_handle: dh,
        loop_handle: handle,
        loop_signal: event_loop.get_signal(),
        backend,
        cursor: crate::cursor::CursorManager::new(saved.pointer.cursor_theme.as_deref(), saved.pointer.cursor_size),
        pointer_config: saved.pointer.clone(),
        show_about: saved.show_about,
        pointer_managed: false,
        input_devices: Vec::new(),
        refresh_mhz,
        rounded_program,
        damage_tracker,
        needs_redraw: true,
        animations: crate::animation::Animations::default(),
        appmenus,
        tray: None,
        notifications: None,
        xwayland: None,
        pending_autostart,
        idle_frame_timer_armed: false,
        frame_pending: false,
        frame_seq: 0,
        swap_interval_disabled: false,
        start_time: Instant::now(),
        compositor_state,
        xdg_shell_state,
        xdg_decoration_state,
        xdg_activation_state,
        shm_state,
        dmabuf_state,
        dmabuf_global,
        seat_state,
        data_device_state,
        primary_selection_state,
        output_manager_state,
        viewporter_state,
        fractional_scale_state,
        cursor_shape_state,
        popups: PopupManager::default(),
        seat,
        keyboard,
        pointer,
        output,
        phys_w,
        phys_h,
        scale_factor: init_scale,
        explicit_scale,
        wayland_socket_name: socket_name.clone(),
        fonts: FontSystem::new(),
        app_registry: AppRegistry::new(),
        shell: {
            let mut shell = MacShell::new(logical_w, logical_h);
            shell.theme = saved.wallpaper;
            shell.dock_zoom_enabled = saved.dock_zoom;
            shell.dnd_enabled = saved.do_not_disturb;
            shell.dnd_schedule = saved.dnd_schedule;
            shell.notify_apps = saved.notify_apps.clone();
            shell.notify_sounds = saved.notify_sounds;
            shell
        },
        windows: Vec::new(),
        next_win_id: 1,
        pointer_location: Point::from((logical_w as f64 * 0.5, logical_h as f64 * 0.5)),
        active_drag: None,
        last_titlebar_click: None,
        pressed_buttons: Vec::new(),
        pointer_focus_dirty: false,
        frame_stats: std::env::var_os("FEATHER_PROFILE").map(|_| crate::render::FrameStats::default()),
        screenshot_request: None,
        screenshot_select: None,
        desktop_tx: None,
        desktop_focus_top: None,
        client_clipboard_mimes: Vec::new(),
        screenshot_tx,
        consumed_buttons: Vec::new(),
        compositor_cursor: None,
        user_interacted: false,
        dock_model_dirty: true,
        dock_pins: Vec::new(),
        dock_locked: saved.dock_locked,
        icon_drag: None,
        active_layout_name: "EN".to_string(),
        active_layout_index: 0,
        keyboard_config: saved.keyboard.clone(),
        preferences: None,
        wallpaper_picker: None,
        system_settings: crate::system_settings::Panel::default(),
        system_preferences: crate::system_preferences::State{browser:saved.default_browser.clone(),terminal:saved.default_terminal.clone().unwrap_or_else(||"feather-terminal.desktop".into()),..Default::default()},
        system_worker: None,
        display_modes: Vec::new(),display_name: String::new(),pending_display: None,display_generation: 0,
        display_preference: saved.display.clone(),
        notes_buffer: "Привет из Пёрышко DE!\nНажмите Alt + Shift для переключения языка (RU / EN).\nНажмите Super + Space для вызова Spotlight.\nНажмите Super + = для переключения HiDPI Retina масштаба!".to_string(),
    };

    state.preferences=Some(crate::settings::Worker::new(&state.loop_handle)?);
    state.wallpaper_picker=Some(crate::wallpaper::Picker::new(&state.loop_handle,state.wayland_socket_name.clone())?);
    state.shell.wallpaper_image=saved.wallpaper_image.clone();
    state.system_worker=Some(crate::system_preferences::Worker::new(&state.loop_handle,saved.default_browser.clone(),saved.default_terminal.clone())?);
    state.refresh_input_source(0);
    state.animations.set_enabled(saved.animations,&state.windows,&state.shell);
    state.refresh_display_choices();

    let raster = crate::raster::RasterSender::new(&state.loop_handle, state.fonts.for_worker())?;
    state.shell.attach_raster(raster);
    state.start_desktop()?;
    state.start_trash()?;
    crate::apps::set_cursor_env(
        crate::cursor::theme_name(state.pointer_config.cursor_theme.as_deref()),
        state.pointer_config.cursor_size,
    );
    if is_standalone && !state.backend.is_drm() {
        crate::labwc::apply(state.pointer_config.clone(), state.scale_factor);
    }
    state.controls=Some(crate::controls::Controls::new(&state.loop_handle)?);
    if is_standalone || use_drm || std::env::var_os("FEATHER_DEBUG_TRAY").is_some() {
        state.tray=Some(crate::tray::Tray::new(&state.loop_handle)?);
    }
    // X11 programs: our own display, satellite only once an X client connects.
    // FEATHER_XWAYLAND=0 turns it off; =1 enables it in a nested run.
    let xwayland = std::env::var("FEATHER_XWAYLAND").ok();
    if xwayland.as_deref() != Some("0") && (is_standalone || use_drm || xwayland.as_deref() == Some("1")) {
        state.start_xwayland(is_standalone || use_drm);
    }
    // Nested runs share the host's session bus: they never serve its notifications.
    if is_standalone || use_drm || std::env::var_os("FEATHER_DEBUG_NOTIFY").is_some() {
        state.notifications=Some(crate::notifications::Notifications::new(&state.loop_handle)?);
    }

    // Start Polkit Authentication Agent (for MouseVPN / pkexec password prompts) and D-Bus env
    spawn_session_services(&socket_name, is_standalone);
    // Own polkit agent in a real session; FEATHER_POLKIT_AGENT=1 forces it (tests),
    // FEATHER_POLKIT=lxqt keeps the external agent.
    let own_agent = std::env::var("FEATHER_POLKIT").as_deref() != Ok("lxqt")
        && (is_standalone || std::env::var_os("FEATHER_POLKIT_AGENT").is_some());
    if own_agent {
        match crate::polkit_agent::Agent::start(&state.loop_handle, is_standalone) {
            Ok(agent) => state.polkit = Some(agent),
            Err(e) => {
                tracing::warn!("polkit: {e}");
                crate::apps::spawn_polkit_fallback(&socket_name);
            }
        }
    } else if is_standalone {
        crate::apps::spawn_polkit_fallback(&socket_name);
    }

    state.dock_pins = saved.dock.clone().unwrap_or_else(|| state.app_registry.default_pins());
    // Keep insertion indices identical to the visible pinned order for legacy configs.
    if !state.dock_pins.iter().any(|id| id == crate::apps::AppRegistry::PERMANENT) {
        state.dock_pins.insert(0, crate::apps::AppRegistry::PERMANENT.into());
    }

    state.pointer_managed = state.backend.is_drm() || crate::labwc::host_pid().is_some();
    if state.show_about {
        state.open_or_focus_builtin(BuiltinWindowKind::AboutFeather);
    }
    if std::env::var_os("FEATHER_DEBUG_ANIMATIONS").is_some() {
        // Deterministic lifecycle exercise in our own compositor; no synthetic host input.
        for step in 0..8u64 {
            state.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(1000 + step * 1000)), move |_, _, state| {
                match step {
                    0 | 1 => {
                        if let Some(win) = state.windows.iter().rev().find(|w| w.mapped) {
                            let target = match state.app_key(win) {
                                AppKey::App(i) => DockTarget::App(i),
                                AppKey::Window(id) => DockTarget::Window(id),
                            };
                            state.execute_shell_action(ShellClickAction::DockPress(target));
                        }
                    }
                    2 => state.shell.open_menu = Some(crate::shell::TopMenu::App),
                    3 => { state.shell.close_menus(); state.shell.control_center_open = true; }
                    4 => { state.shell.close_menus(); state.shell.spotlight_open = true; }
                    5 => { state.shell.close_menus(); state.toggle_launchpad(); }
                    6 => state.toggle_launchpad(),
                    _ => {},
                }
                state.shell.overlay_dirty = true;
                state.needs_redraw = true;
                TimeoutAction::Drop
            })?;
        }
    }
    if std::env::var_os("FEATHER_DEBUG_RASTER_STRESS").is_some() {
        // Own-state stress only: change snapshots while jobs are queued, no host input.
        for step in 0..18u64 {
            state.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(300 + step * 60)), move |_, _, state| {
                if step == 17 {
                    set_appearance(Appearance::Dark);
                    state.shell.theme = WallpaperTheme::TahoeNight;
                    state.apply_hidpi_scale(1.5);
                    state.shell.launchpad = Some(crate::launchpad::Launchpad::new(&state.app_registry, state.shell.screen_w, state.shell.screen_h));
                } else {
                    match step % 3 {
                        0 => { state.shell.theme = state.shell.theme.next(); state.shell.invalidate_all(); }
                        1 => state.apply_hidpi_scale(if step % 2 == 0 { 2.0 } else { 1.25 }),
                        _ => {
                            if state.shell.launchpad.is_some() { state.toggle_launchpad(); }
                            state.toggle_launchpad();
                            if let Some(lp) = state.shell.launchpad.as_mut() {
                                lp.set_query(&state.app_registry, if step % 2 == 0 { "term".into() } else { String::new() });
                                lp.set_page(1);
                            }
                        }
                    }
                }
                state.needs_redraw = true;
                TimeoutAction::Drop
            })?;
        }
    }
    if let Ok(path) = std::env::var("FEATHER_DEBUG_APPMENU") {
        let activate = std::env::var_os("FEATHER_DEBUG_APPMENU_ACTIVATE").is_some();
        let delay = std::env::var("FEATHER_DEBUG_APPMENU_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(1500);
        state.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(delay)), move |_, _, state| {
            let action = state.shell.debug_client_menu_path(&path, activate, &state.app_registry);
            if let Some(action) = action { state.execute_shell_action(action); }
            else { tracing::warn!("Отладочное меню не найдено: {path}"); }
            state.needs_redraw = true;
            TimeoutAction::Drop
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_DESKTOP").is_some() {
        state.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(1500)), |_, _, state| {
            let screen = (state.shell.screen_w, state.shell.screen_h);
            state.shell.desktop.debug_select(screen);
            state.needs_redraw = true;
            TimeoutAction::Drop
        })?;
    }
    debug_checks::install(&mut state)?;
    if std::env::var_os("FEATHER_DEBUG_DND").is_some() && std::env::var("FEATHER_DEBUG_DND").as_deref() != Ok("resize-all") {
        // Desktop "drag-me.txt" → Files window, then Files' first item → empty desktop.
        // Pointer events go only to our own nested compositor and its test client.
        // FEATHER_DEBUG_DND=move: only drag the desktop icon to another place on the desktop
        let mode = std::env::var("FEATHER_DEBUG_DND").unwrap_or_default();
        let steps: Vec<(u64, &'static str)> = if mode == "resize" {
            vec![(3000, "edge-press"), (3300, "drag-right"), (3700, "release"), (4500, "report")]
        } else if mode == "menu" {
            vec![(3000, "menu")]
        } else if mode == "move" {
            vec![(3000, "desk-press"), (3400, "move-left"), (3800, "release"), (5000, "report")]
        } else {
            vec![(4000, "desk-press"), (4600, "move-files"), (5200, "release"), (7000, "files-press"), (7600, "move-desk"), (8200, "release"), (9500, "report")]
        };
        for (at, step) in steps {
            state.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(at)), move |_, _, state| {
                let files_win = state.windows.iter().find(|w| w.app_id == "feather-files" && w.is_visible()).map(|w| (w.x as f64, w.y as f64, w.width as f64, w.height as f64));
                match step {
                    "desk-press" => {
                        let screen = (state.shell.screen_w, state.shell.screen_h);
                        state.shell.desktop.ensure_layout(screen);
                        if let Some(i) = state.shell.desktop.debug_index("drag-me.txt") {
                            let (x, y) = state.shell.desktop.position(i, screen);
                            state.debug_pointer_motion(x + 48.0, y + 30.0);
                            state.debug_pointer_button(0x110, true);
                        }
                    }
                    "files-press" => {
                        if let Some((x, y, _, _)) = files_win {
                            // First icon of the grid: sidebar 220 + padding, toolbar 52 + 10
                            state.debug_pointer_motion(x + 220.0 + 8.0 + 55.0, y + 52.0 + 10.0 + 38.0);
                            state.debug_pointer_button(0x110, true);
                        }
                    }
                    "move-left" => {
                        let from = (state.pointer_location.x, state.pointer_location.y);
                        for k in 1..=10 {
                            state.debug_pointer_motion(from.0 - 20.0 * k as f64, from.1 + 10.0 * k as f64);
                        }
                    }
                    "move-files" | "move-desk" => {
                        let from = (state.pointer_location.x, state.pointer_location.y);
                        let to = match (step, files_win) {
                            ("move-files", Some((x, y, w, h))) => (x + w * 0.6, y + h * 0.6),
                            // Empty desktop above the Files window
                            (_, Some((x, y, w, _))) => (x + w * 0.3, (y - 70.0).max(80.0)),
                            _ => (300.0, 120.0),
                        };
                        for k in 1..=12 {
                            let t = k as f64 / 12.0;
                            state.debug_pointer_motion(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
                        }
                    }
                    "release" => state.debug_pointer_button(0x110, false),
                    "edge-press" => {
                        if let Some(w) = state.windows.iter().find(|w| w.app_id == "feather-terminal" && w.is_visible()) {
                            let (x, y) = ((w.x + w.width) as f64 - 2.0, w.y as f64 + w.height as f64 * 0.5);
                            state.debug_pointer_motion(x, y);
                            state.debug_pointer_button(0x110, true);
                        }
                    }
                    "drag-right" => {
                        let from = (state.pointer_location.x, state.pointer_location.y);
                        for k in 1..=10 {
                            state.debug_pointer_motion(from.0 - 15.0 * k as f64, from.1 + 8.0 * k as f64);
                        }
                    }
                    "menu" => {
                        let screen = (state.shell.screen_w, state.shell.screen_h);
                        state.shell.desktop.ensure_layout(screen);
                        if let Some(i) = state.shell.desktop.debug_index("drag-me.txt") {
                            let (x, y) = state.shell.desktop.position(i, screen);
                            state.debug_pointer_motion(x + 48.0, y + 30.0);
                            state.debug_pointer_button(0x111, true);
                            state.debug_pointer_button(0x111, false);
                        }
                    }
                    _ => eprintln!("[feather-dnd-test] done"),
                }
                TimeoutAction::Drop
            })?;
        }
    }
    // Screenshot pipeline test without synthetic input: FEATHER_DEBUG_CAPTURE=screen|window|area
    if let Ok(kind) = std::env::var("FEATHER_DEBUG_CAPTURE") {
        state.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(2500)), move |_, _, state| {
            if kind == "select" {
                // Shows the selection UI with a fixed anchor; the pointer is where the host left it
                state.start_screenshot_selection();
                if let Some(sel) = state.screenshot_select.as_mut() { sel.debug_anchor((120.0, 90.0)); }
                state.screenshot_motion();
                return TimeoutAction::Drop;
            }
            let target = match kind.as_str() {
                "window" => state.window_target(|_| true),
                "area" => Some(crate::screenshot::Target::Area { x: 100.0, y: 50.0, w: 300.5, h: 200.0, radius: 0.0 }),
                _ => None,
            };
            state.take_screenshot(target.unwrap_or(crate::screenshot::Target::Screen));
            TimeoutAction::Drop
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_LOCK").is_some() {state.lock_session();}
    // Visual tests: FEATHER_DEBUG_SCREENSHOT=/path.png captures our own output after 2.5 s
    if let Some(path) = std::env::var_os("FEATHER_DEBUG_SCREENSHOT") {
        let path = std::path::PathBuf::from(path);
        let delay = std::env::var("FEATHER_DEBUG_SCREENSHOT_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(2500);
        state.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(delay)), move |_, _, state| {
            state.screenshot_request = Some(path.clone());
            // Full redraw so the back buffer holds the complete frame
            state.damage_tracker =
                OutputDamageTracker::new((state.phys_w, state.phys_h), 1.0, Transform::Flipped180);
            state.needs_redraw = true;
            TimeoutAction::Drop
        })?;
    }
    // Development aid for screenshot tests: FEATHER_DEBUG_OPEN=launchpad
    match std::env::var("FEATHER_DEBUG_OPEN").as_deref() {
        Ok("launchpad") => state.toggle_launchpad(),
        Ok("menu-feather") => state.shell.open_menu = Some(crate::shell::TopMenu::Feather),
        Ok("menu-app") => state.shell.open_menu = Some(crate::shell::TopMenu::App),
        Ok("menu-window") => state.shell.open_menu = Some(crate::shell::TopMenu::Window),
        Ok("cc") => state.shell.control_center_open = true,
        Ok("center") => state.shell.toggle_center(),
        Ok("dock-minimized") => {
            if let Some(w) = state.windows.last_mut() {
                w.minimized = true;
            }
            state.dock_model_dirty = true;
            state.refresh_focus();
        }
        Ok("cc-wifi") => {state.shell.control_center_open=true;state.shell.cc.open(crate::control_panel::Page::Wifi);}
        Ok("cc-sound") => {state.shell.control_center_open=true;state.shell.cc.open(crate::control_panel::Page::Sound);}
        _ => {}
    }


    state.start_power();
    // Event-driven main loop: sleeps until input, a client request or a timer fires,
    // and only renders when something actually changed.
    event_loop.run(None, &mut state, |state| {
        if let Some(controls)=&mut state.controls {
            controls.sync(state.shell.control_center_open,state.shell.control_center_open && matches!(state.shell.cc.page,crate::control_panel::Page::Wifi|crate::control_panel::Page::Password));
        }
        if !state.shell.control_center_open {state.shell.cc.close();}
        let password=state.shell.control_center_open && state.shell.cc.page==crate::control_panel::Page::Password;
        if password!=state.controls_password_focus {state.controls_password_focus=password;state.refresh_focus();}
        state.flush_appmenu_commands();
        if let Some(tray)=&mut state.tray{tray.sync_menu(state.shell.tray_open_ids());}
        if state.pointer_focus_dirty {
            state.refresh_pointer_focus();
        }
        state.refresh_lock();
        state.sync_idle_inhibit();
        state.flush_display_mode();
        state.render_if_needed();
        if let Err(err) = state.display_handle.flush_clients() {
            tracing::warn!("Ошибка flush_clients: {err}");
        }
    })?;

    Ok(())
}
