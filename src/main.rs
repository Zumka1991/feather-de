mod apps;
mod backend;
mod cursor;
mod drm_backend;
mod handlers;
mod icons;
mod input;
mod launchpad;
mod render;
mod rounded;
mod settings;
mod shell;
mod text;
mod theme;
mod window;

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
    /// Presses handled by the compositor whose release must not reach clients
    pub consumed_buttons: Vec<u32>,
    pub compositor_cursor: Option<CursorIcon>,
    pub user_interacted: bool,
    /// Windows were added/removed/restacked: rebuild the Dock model on the next frame
    pub dock_model_dirty: bool,
    /// Pinned Dock apps (registry ids), in Dock order
    pub dock_pins: Vec<String>,
    /// App icon being pressed/dragged from the Dock or Launchpad
    pub icon_drag: Option<IconDrag>,

    pub active_layout_name: String,
    pub notes_buffer: String,
}

impl FeatherCompositor {
    /// Single source of truth for stacking → activation → keyboard focus.
    /// The topmost visible window is active; only Wayland windows receive keyboard focus.
    pub fn refresh_focus(&mut self) {
        let top = self.windows.iter().rposition(|w| w.is_visible());
        for (i, w) in self.windows.iter_mut().enumerate() {
            w.set_activated(Some(i) == top);
        }
        let target = top.and_then(|i| self.windows[i].wl_surface().cloned());
        let keyboard = self.keyboard.clone();
        if keyboard.current_focus() != target {
            keyboard.set_focus(self, target, SERIAL_COUNTER.next_serial());
        }
        self.shell.topbar_dirty = true;
        self.pointer_focus_dirty = true;
        self.dock_model_dirty = true;
        self.needs_redraw = true;
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
                running: keys.iter().any(|(k, _)| *k == AppKey::App(i)),
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
                running: true,
                pinned: false,
            });
        }

        for (key, w) in keys.iter().filter(|(_, w)| w.minimized) {
            let (_, icon) = self.icon_for_key(*key, Some(w));
            items.push(DockItem {
                target: DockTarget::Minimized(w.id),
                label: w.title.clone(),
                icon,
                running: false,
                pinned: false,
            });
        }
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
                let app = &self.app_registry.apps[i];
                let action = ShellClickAction::LaunchApp(app.action.clone(), app.name.clone());
                self.execute_shell_action(action);
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
        crate::settings::Settings {
            appearance: appearance(),
            wallpaper: self.shell.theme,
            scale: self.explicit_scale,
            dock_zoom: self.shell.dock_zoom_enabled,
            dock: Some(self.dock_pins.clone()),
        }
        .save();
    }

    /// Registry indices of pinned apps, in Dock order (unknown ids are skipped)
    pub fn pinned_indices(&self) -> Vec<usize> {
        self.dock_pins
            .iter()
            .filter_map(|id| self.app_registry.index_of(id))
            .collect()
    }

    pub fn is_pinned(&self, app: usize) -> bool {
        let id = &self.app_registry.apps[app].id;
        self.dock_pins.iter().any(|p| p == id)
    }

    pub fn pin_app(&mut self, id: &str, at: Option<usize>) {
        self.dock_pins.retain(|p| p != id);
        let at = at.unwrap_or(self.dock_pins.len()).min(self.dock_pins.len());
        self.dock_pins.insert(at, id.to_string());
        self.dock_model_dirty = true;
        self.save_settings();
    }

    pub fn unpin_app(&mut self, id: &str) {
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
        let gap = self
            .shell
            .dock_in_drop_zone(pos.x, pos.y)
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

        if !drag.active {
            self.execute_shell_action(drag.click);
            return;
        }
        let pos = self.pointer_location;
        let in_dock = self.shell.dock_in_drop_zone(pos.x, pos.y);
        match drag.source {
            DragSource::DockApp { id, pinned } => {
                if in_dock {
                    self.pin_app(&id, gap);
                } else if pinned {
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

    /// Right click on a Dock icon
    pub fn open_dock_menu_for(&mut self, target: DockTarget, anchor: R) {
        let running = match target {
            DockTarget::App(i) => self.windows.iter().any(|w| w.mapped && self.app_key(w) == AppKey::App(i)),
            DockTarget::Window(_) | DockTarget::Minimized(_) => true,
        };
        let mut items = vec![(
            if running { "Показать".to_string() } else { "Открыть".to_string() },
            DockMenuAction::Open,
        )];
        if let DockTarget::App(i) = target {
            if self.is_pinned(i) {
                items.push(("Удалить из Dock".to_string(), DockMenuAction::RemoveFromDock));
            } else {
                items.push(("Оставить в Dock".to_string(), DockMenuAction::KeepInDock));
            }
        }
        if running {
            items.push(("Завершить".to_string(), DockMenuAction::Quit));
        }
        self.shell.open_dock_menu(target, anchor, items, &mut self.fonts);
    }

    fn dock_target_key(&self, target: DockTarget) -> Option<AppKey> {
        match target {
            DockTarget::App(i) => Some(AppKey::App(i)),
            DockTarget::Window(id) | DockTarget::Minimized(id) => {
                self.windows.iter().find(|w| w.id == id).map(|w| self.app_key(w))
            }
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

    pub fn execute_shell_action(&mut self, action: ShellClickAction) {
        self.needs_redraw = true;
        match action {
            ShellClickAction::None => {}
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
                // Only reached when a press is executed directly (no drag tracking)
                let action = match target {
                    DockTarget::App(i) => ShellClickAction::ActivateApp(i),
                    DockTarget::Window(id) => ShellClickAction::ActivateWindow(id),
                    DockTarget::Minimized(id) => ShellClickAction::RestoreMinimizedWindow(id),
                };
                self.execute_shell_action(action);
            }
            ShellClickAction::DockMenu(action, target) => match action {
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
            },
            ShellClickAction::ActivateApp(idx) => self.activate_app(AppKey::App(idx)),
            ShellClickAction::ActivateWindow(win_id) => {
                if let Some(w) = self.windows.iter().find(|w| w.id == win_id) {
                    let key = self.app_key(w);
                    self.activate_app(key);
                }
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
                let kbd = self.keyboard.clone();
                let mut new_idx = 0;
                kbd.with_xkb_state(self, |mut ctx| {
                    ctx.cycle_next_layout();
                    new_idx = ctx.xkb().lock().unwrap().active_layout().0;
                });
                self.active_layout_name = if new_idx == 1 { "RU" } else { "EN" }.to_string();
                self.shell.topbar_dirty = true;
                self.shell.overlay_dirty = true;
                self.mark_builtin_windows_dirty();
            }
            ShellClickAction::CycleTheme => {
                self.shell.theme = self.shell.theme.next();
                self.shell.wallpaper_dirty = true;
                self.shell.overlay_dirty = true;
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
            WinitEvent::Resized { size, scale_factor } => self.on_resized(size, scale_factor),
            WinitEvent::Input(event) => self.handle_input(event),
            // Rendering is driven by our own damage; the host's redraw event only tells us
            // that the previous frame was presented and the next one may go out
            WinitEvent::Redraw => self.on_host_frame(),
            WinitEvent::CloseRequested => self.loop_signal.stop(),
            WinitEvent::Focus(false) => self.release_all_input(),
            WinitEvent::Focus(true) => {}
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

    let compositor_state = CompositorState::new::<FeatherCompositor>(&dh);
    let xdg_shell_state = XdgShellState::new::<FeatherCompositor>(&dh);
    let xdg_decoration_state = XdgDecorationState::new::<FeatherCompositor>(&dh);
    let xdg_activation_state = XdgActivationState::new::<FeatherCompositor>(&dh);
    let shm_state = ShmState::new::<FeatherCompositor>(&dh, vec![]);
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

    let mut winit_loop = None;
    let mut drm_info = None;
    let mut backend = if use_drm {
        let d = crate::drm_backend::init(&event_loop.handle())?;
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

    let saved = crate::settings::Settings::load();
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

    let xkb_config = XkbConfig {
        rules: "",
        model: "pc105",
        layout: "us,ru",
        variant: "",
        options: Some("grp:alt_shift_toggle".to_string()),
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
        if state.shell.tick() {
            state.needs_redraw = true;
        }
        state.prune_dead_windows();
        TimeoutAction::ToDuration(Duration::from_secs(1))
    })?;

    // Only used by the nested backend; DRM has its own damage tracking per output
    let damage_tracker = OutputDamageTracker::new(initial_size, 1.0, Transform::Flipped180);

    let mut state = FeatherCompositor {
        display_handle: dh,
        loop_handle: handle,
        loop_signal: event_loop.get_signal(),
        backend,
        cursor: crate::cursor::CursorManager::new(),
        refresh_mhz,
        rounded_program,
        damage_tracker,
        needs_redraw: true,
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
        consumed_buttons: Vec::new(),
        compositor_cursor: None,
        user_interacted: false,
        dock_model_dirty: true,
        dock_pins: Vec::new(),
        icon_drag: None,
        active_layout_name: "EN".to_string(),
        notes_buffer: "Привет из Пёрышко DE!\nНажмите Alt + Shift для переключения языка (RU / EN).\nНажмите Super + Space для вызова Spotlight.\nНажмите Super + = для переключения HiDPI Retina масштаба!".to_string(),
    };

    // Start Polkit Authentication Agent (for MouseVPN / pkexec password prompts) and D-Bus env
    spawn_session_services(&socket_name, is_standalone);

    state.dock_pins = saved.dock.clone().unwrap_or_else(|| state.app_registry.default_pins());

    state.open_or_focus_builtin(BuiltinWindowKind::AboutFeather);
    // Visual tests: FEATHER_DEBUG_SCREENSHOT=/path.png captures our own output after 2.5 s
    if let Some(path) = std::env::var_os("FEATHER_DEBUG_SCREENSHOT") {
        let path = std::path::PathBuf::from(path);
        state.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(2500)), move |_, _, state| {
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
        _ => {}
    }

    if let Some(pos) = args.iter().position(|a| a == "--autostart") {
        if let Some(cmd) = args.get(pos + 1) {
            spawn_wayland_command(cmd, &socket_name);
        }
    }

    // Event-driven main loop: sleeps until input, a client request or a timer fires,
    // and only renders when something actually changed.
    event_loop.run(None, &mut state, |state| {
        if state.pointer_focus_dirty {
            state.refresh_pointer_focus();
        }
        state.render_if_needed();
        if let Err(err) = state.display_handle.flush_clients() {
            tracing::warn!("Ошибка flush_clients: {err}");
        }
    })?;

    Ok(())
}
