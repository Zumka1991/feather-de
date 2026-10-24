use std::time::Instant;

use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
            KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
        },
    },
    desktop::{find_popup_root_surface, PopupManager},
    input::{
        keyboard::{keysyms, FilterResult},
        pointer::{AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::compositor::get_parent,
};

use crate::{
    apps::{AppAction, BuiltinWindowKind},
    launchpad::LaunchpadHit,
    shell::{AppIcon, DockTarget, ShellClickAction},
    theme::{TITLEBAR_HEIGHT, TOP_BAR_HEIGHT},
    window::{Edges, ResizeAnchor, WindowBackend, MIN_WINDOW_H, MIN_WINDOW_W},
    DragOperation, DragSource, FeatherCompositor,
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const DOUBLE_CLICK_MS: u128 = 350;

/// Actions decided inside the keyboard filter. They run after it returns, because changing
/// keyboard focus from inside `KeyboardHandle::input` would deadlock on the keyboard lock.
enum KeyAction {
    Shell(ShellClickAction),
    CloseTop,
    MinimizeTop,
    ToggleMaximizeTop,
    /// Alt/Super+Tab: open the app switcher or move the selection
    SwitcherStep { reverse: bool },
    SwitcherCommit,
    SwitcherCancel,
    SwitchVt(i32),
    /// Print: screen, Shift+Print: region/window selection, Alt+Print: active window
    Screenshot { shift: bool, alt: bool },
    Auth(crate::auth_dialog::Cmd),
    Lock,
    Unlock,
}

/// Delay before the Alt+Tab panel appears: short enough to feel instant when Alt is held,
/// long enough that a quick tap (switch to the previous app) doesn't flash the panel
const SWITCHER_SHOW_DELAY: std::time::Duration = std::time::Duration::from_millis(45);

fn cursor_for_edges(edges: Edges) -> CursorIcon {
    match (edges.left, edges.right, edges.top, edges.bottom) {
        (true, _, true, _) => CursorIcon::NwResize,
        (_, true, true, _) => CursorIcon::NeResize,
        (true, _, _, true) => CursorIcon::SwResize,
        (_, true, _, true) => CursorIcon::SeResize,
        (true, ..) | (_, true, ..) => CursorIcon::EwResize,
        _ => CursorIcon::NsResize,
    }
}

impl FeatherCompositor {
    /// Input from either backend (winit when nested, libinput on DRM)
    pub fn handle_input<B: InputBackend>(&mut self, event: InputEvent<B>) {
        // The event which wakes a blank screen is consumed in full.
        let activity=matches!(&event,InputEvent::Keyboard{..}|InputEvent::PointerMotion{..}|InputEvent::PointerMotionAbsolute{..}|InputEvent::PointerButton{..}|InputEvent::PointerAxis{..});
        if activity && self.activity(){return;}
        self.needs_redraw = true;
        match event {
            InputEvent::Keyboard { event } => self.on_keyboard::<B>(event),
            InputEvent::PointerMotion { event } => {
                // Relative motion (mice/touchpads on DRM): libinput already applied acceleration
                let delta = event.delta();
                let max_x = (self.shell.screen_w - 1) as f64;
                let max_y = (self.shell.screen_h - 1) as f64;
                let pos = Point::from((
                    (self.pointer_location.x + delta.x).clamp(0.0, max_x),
                    (self.pointer_location.y + delta.y).clamp(0.0, max_y),
                ));
                self.on_pointer_motion(pos, Event::time_msec(&event));
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let pos = event.position_transformed((self.shell.screen_w, self.shell.screen_h).into());
                self.on_pointer_motion(pos, Event::time_msec(&event));
            }
            InputEvent::PointerButton { event } => {
                self.on_pointer_button(event.button_code(), event.state(), Event::time_msec(&event));
            }
            InputEvent::PointerAxis { event } => self.on_pointer_axis::<B>(event),
            _ => {}
        }
    }

    fn on_keyboard<B: InputBackend>(&mut self, event: B::KeyboardKeyEvent) {
        let keycode = event.key_code();
        let key_state = event.state();
        self.process_keyboard_key(keycode,key_state,Event::time_msec(&event));
    }
    pub fn debug_keyboard_key(&mut self,evdev:u32,pressed:bool){
        self.process_keyboard_key((evdev+8).into(),if pressed{KeyState::Pressed}else{KeyState::Released},self.start_time.elapsed().as_millis() as u32);
    }
    fn process_keyboard_key(&mut self,keycode:smithay::input::keyboard::Keycode,key_state:KeyState,time:u32){
        if key_state == KeyState::Pressed {
            if let Some(stats) = self.frame_stats.as_mut() {
                stats.record_key();
            }
        }
        let serial = SERIAL_COUNTER.next_serial();
        let mut deferred: Option<KeyAction> = None;

        let keyboard = self.keyboard.clone();
        keyboard.input::<(), _>(self, keycode, key_state, serial, time, |st, mods, keysym_handle| {
            let layout_idx = keysym_handle.xkb().lock().unwrap().active_layout().0;
            if st.active_layout_index!=layout_idx as usize{st.refresh_input_source(layout_idx as usize);}
            // Nothing in the desktop (including emergency quit and screenshots) runs locked.
            if st.lock.active {
                if key_state==KeyState::Pressed && mods.ctrl && mods.alt {
                    let sym=keysym_handle.modified_sym().raw();
                    if (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&sym) {
                        deferred=Some(KeyAction::SwitchVt((sym-keysyms::KEY_XF86Switch_VT_1+1) as i32));return FilterResult::Intercept(());
                    }
                    if (keysyms::KEY_F1..=keysyms::KEY_F12).contains(&sym) {
                        deferred=Some(KeyAction::SwitchVt((sym-keysyms::KEY_F1+1) as i32));
                        return FilterResult::Intercept(());
                    }
                }
                if st.lock.external() {return FilterResult::Forward;}
                if key_state==KeyState::Pressed {
                    let sym=keysym_handle.modified_sym();
                    if st.lock.key(sym.raw(),sym.key_char(),mods.ctrl,mods.shift,mods.caps_lock) {deferred=Some(KeyAction::Unlock);}
                    st.needs_redraw=true;
                }
                return FilterResult::Intercept(());
            }
            if key_state==KeyState::Pressed && mods.logo && keysym_handle.raw_latin_sym_or_raw_current_sym().is_some_and(|sym|sym.raw()==keysyms::KEY_l) {
                deferred=Some(KeyAction::Lock);return FilterResult::Intercept(());
            }
            let system_chord=mods.ctrl&&mods.alt;
            // The password dialog owns the keyboard; only VT/system chords pass.
            if st.auth.is_open() && !system_chord {
                if key_state==KeyState::Pressed {
                    let sym=keysym_handle.modified_sym();
                    if let Some(cmd)=st.auth.key(sym.raw(),sym.key_char(),mods.ctrl,mods.caps_lock) {deferred=Some(KeyAction::Auth(cmd));}
                    st.needs_redraw=true;
                }
                return FilterResult::Intercept(());
            }
            if st.shell.control_center_open && st.shell.cc.page==crate::control_panel::Page::Password && !system_chord {
                if key_state==KeyState::Pressed {
                    let sym=keysym_handle.modified_sym();
                    if let Some(action)=st.shell.cc.key(sym.raw(),sym.key_char(),mods.ctrl,mods.shift) {deferred=Some(KeyAction::Shell(action));}
                    st.shell.overlay_dirty=true;
                }
                return FilterResult::Intercept(());
            }
            // App switcher: active while Alt (or Super) is held
            if st.shell.switcher.is_some() {
                let sym = keysym_handle.modified_sym().raw();
                if key_state == KeyState::Released {
                    if !mods.alt && !mods.logo {
                        deferred = Some(KeyAction::SwitcherCommit);
                        // If focus moves to another app, the old one must not see a lone Alt
                        // release (Firefox & co. would open their menu bar); it gets
                        // wl_keyboard.leave instead, which resets its key state.
                        return if st.switcher_changes_focus() {
                            FilterResult::Intercept(())
                        } else {
                            FilterResult::Forward
                        };
                    }
                    // Keys the switcher consumed must not be released into the client
                    return match sym {
                        keysyms::KEY_Tab
                        | keysyms::KEY_ISO_Left_Tab
                        | keysyms::KEY_Left
                        | keysyms::KEY_Right
                        | keysyms::KEY_grave
                        | keysyms::KEY_Escape
                        | keysyms::KEY_Return => FilterResult::Intercept(()),
                        _ => FilterResult::Forward,
                    };
                }
                match sym {
                    keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab => {
                        deferred = Some(KeyAction::SwitcherStep {
                            reverse: mods.shift || sym == keysyms::KEY_ISO_Left_Tab,
                        });
                    }
                    keysyms::KEY_Right | keysyms::KEY_grave => {
                        deferred = Some(KeyAction::SwitcherStep { reverse: false })
                    }
                    keysyms::KEY_Left => deferred = Some(KeyAction::SwitcherStep { reverse: true }),
                    keysyms::KEY_Escape => deferred = Some(KeyAction::SwitcherCancel),
                    keysyms::KEY_Return => deferred = Some(KeyAction::SwitcherCommit),
                    _ => {}
                }
                return FilterResult::Intercept(());
            }
            if key_state == KeyState::Pressed {
                let raw = keysym_handle.modified_sym().raw();
                // Ctrl+Alt+F1…F12 (XF86Switch_VT_n) and the emergency exit Ctrl+Alt+Backspace
                if (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&raw) {
                    deferred = Some(KeyAction::SwitchVt((raw - keysyms::KEY_XF86Switch_VT_1 + 1) as i32));
                    return FilterResult::Intercept(());
                }
                // Alt+Print arrives as Sys_Req on most layouts
                if raw == keysyms::KEY_Print || raw == keysyms::KEY_Sys_Req {
                    deferred = Some(KeyAction::Screenshot { shift: mods.shift, alt: mods.alt });
                    return FilterResult::Intercept(());
                }
                if st.screenshot_select.is_some() && raw == keysyms::KEY_Escape {
                    deferred = Some(KeyAction::Screenshot { shift: false, alt: false });
                    return FilterResult::Intercept(());
                }
                if mods.ctrl && mods.alt && raw == keysyms::KEY_BackSpace {
                    deferred = Some(KeyAction::Shell(ShellClickAction::QuitCompositor));
                    return FilterResult::Intercept(());
                }
            }
            if key_state == KeyState::Pressed && (mods.alt || mods.logo) {
                let raw = keysym_handle.modified_sym().raw();
                if raw == keysyms::KEY_Tab || raw == keysyms::KEY_ISO_Left_Tab {
                    deferred = Some(KeyAction::SwitcherStep {
                        reverse: mods.shift || raw == keysyms::KEY_ISO_Left_Tab,
                    });
                    return FilterResult::Intercept(());
                }
            }


            if key_state != KeyState::Pressed {
                return FilterResult::Forward;
            }

            let sym = keysym_handle.modified_sym();
            let latin_sym = keysym_handle.raw_latin_sym_or_raw_current_sym().unwrap_or(sym);
            // Keys for the desktop icons when no window has the keyboard
            let shell_idle = st.shell.open_menu.is_none() && !st.shell.spotlight_open && st.shell.launchpad.is_none()
                && !st.shell.control_center_open && st.shell.switcher.is_none();
            if shell_idle && st.desktop_key(latin_sym.raw(), mods.ctrl) {
                return FilterResult::Intercept(());
            }
            if st.shell.open_menu.is_some() && sym.raw() == keysyms::KEY_Escape {
                st.shell.close_menus();
                return FilterResult::Intercept(());
            }
            if st.shell.open_menu==Some(crate::shell::TopMenu::Input){
                let direction=match sym.raw(){keysyms::KEY_Down=>Some(1),keysyms::KEY_Up=>Some(-1),keysyms::KEY_Return|keysyms::KEY_KP_Enter=>Some(0),_=>None};
                if let Some(direction)=direction{if let Some(action)=st.shell.input_menu_key(direction){deferred=Some(KeyAction::Shell(action));}return FilterResult::Intercept(());}
            }
            if matches!(st.shell.open_menu, Some(crate::shell::TopMenu::Client(_)|crate::shell::TopMenu::Tray(_))) {
                let direction = match sym.raw() {
                    keysyms::KEY_Down => Some(1), keysyms::KEY_Up => Some(-1),
                    keysyms::KEY_Right => Some(2), keysyms::KEY_Left => Some(-2),
                    keysyms::KEY_Return | keysyms::KEY_KP_Enter => Some(0), _ => None,
                };
                if let Some(direction) = direction {
                    if let Some(action) = st.shell.client_menu_key(direction) { deferred = Some(KeyAction::Shell(action)); }
                    return FilterResult::Intercept(());
                }
            }

            if mods.logo {
                if mods.shift && matches!(latin_sym.raw(), keysyms::KEY_r | keysyms::KEY_R) {
                    deferred = Some(KeyAction::Shell(ShellClickAction::HotReloadShell));
                    return FilterResult::Intercept(());
                }
                match latin_sym.raw() {
                    keysyms::KEY_equal | keysyms::KEY_plus | keysyms::KEY_minus => {
                        deferred = Some(KeyAction::Shell(ShellClickAction::CycleHiDpiScale));
                        return FilterResult::Intercept(());
                    }
                    keysyms::KEY_space => {
                        st.shell.spotlight_open = !st.shell.spotlight_open;
                        st.shell.control_center_open = false;
                        st.shell.open_menu = None;
                        st.shell.overlay_dirty = true;
                        st.shell.topbar_dirty = true;
                        return FilterResult::Intercept(());
                    }
                    // macOS: ⌘Q quits the app, ⌘W closes the window, ⌘H hides the app
                    keysyms::KEY_q => {
                        deferred = Some(KeyAction::Shell(ShellClickAction::QuitFocusedApp));
                        return FilterResult::Intercept(());
                    }
                    keysyms::KEY_w => {
                        deferred = Some(KeyAction::CloseTop);
                        return FilterResult::Intercept(());
                    }
                    keysyms::KEY_h => {
                        deferred = Some(KeyAction::Shell(if mods.alt {
                            ShellClickAction::HideOthers
                        } else {
                            ShellClickAction::HideApp
                        }));
                        return FilterResult::Intercept(());
                    }
                    keysyms::KEY_m => {
                        deferred = Some(KeyAction::MinimizeTop);
                        return FilterResult::Intercept(());
                    }
                    keysyms::KEY_f => {
                        deferred = Some(KeyAction::ToggleMaximizeTop);
                        return FilterResult::Intercept(());
                    }
                    keysyms::KEY_Return | keysyms::KEY_t => {
                        deferred = Some(KeyAction::Shell(ShellClickAction::LaunchApp(
                            AppAction::Exec(st.system_preferences.terminal_command()),
                            "Терминал".to_string(),
                        )));
                        return FilterResult::Intercept(());
                    }
                    // Super+E: a new Files window at home (as Win+E opens Explorer)
                    keysyms::KEY_e => {
                        deferred = Some(KeyAction::Shell(ShellClickAction::LaunchApp(
                            AppAction::Exec(crate::apps::files_command()),
                            "Файлы".to_string(),
                        )));
                        return FilterResult::Intercept(());
                    }
                    keysyms::KEY_n => {
                        st.shell.control_center_open = !st.shell.control_center_open;
                        st.shell.overlay_dirty = true;
                        return FilterResult::Intercept(());
                    }
                    _ => {}
                }
            }

            if mods.logo && matches!(latin_sym.raw(), keysyms::KEY_a | keysyms::KEY_A) {
                deferred = Some(KeyAction::Shell(ShellClickAction::LaunchApp(
                    AppAction::OpenLaunchpad,
                    "Приложения".to_string(),
                )));
                return FilterResult::Intercept(());
            }

            if st.shell.launchpad.is_some() {
                let registry = &st.app_registry;
                let lp = st.shell.launchpad.as_mut().unwrap();
                match sym.raw() {
                    keysyms::KEY_Escape => st.shell.launchpad = None,
                    keysyms::KEY_Left | keysyms::KEY_Right if lp.input.is_empty() => {
                        if sym.raw() == keysyms::KEY_Left {
                            lp.set_page(lp.page.saturating_sub(1));
                        } else {
                            lp.set_page(lp.page + 1);
                        }
                    }
                    keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                        if let Some(&app) = lp.apps.first() {
                            deferred = Some(KeyAction::Shell(ShellClickAction::LaunchpadLaunch(app)));
                        }
                        st.shell.launchpad = None;
                    }
                    raw => {
                        let utf8 = sym.key_char().map(String::from);
                        let key = if mods.ctrl { latin_sym.raw() } else { raw };
                        match lp.input.key(key, utf8.as_deref(), mods.ctrl, mods.shift) {
                            feather_ui::input::Edit::Changed => {
                                let q = lp.input.text().to_owned();
                                lp.set_query(registry, q);
                            }
                            feather_ui::input::Edit::Moved => lp.mark_search_dirty(),
                            feather_ui::input::Edit::Ignored => {}
                        }
                    }
                }
                return FilterResult::Intercept(());
            }

            if st.shell.spotlight_open {
                match sym.raw() {
                    keysyms::KEY_Escape => {
                        st.shell.spotlight_open = false;
                        st.shell.overlay_dirty = true;
                        st.shell.topbar_dirty = true;
                    }
                    keysyms::KEY_Up => {
                        st.shell.spotlight_selected = st.shell.spotlight_selected.saturating_sub(1);
                        st.shell.overlay_dirty = true;
                    }
                    keysyms::KEY_Down => {
                        let count = st.app_registry.search(&st.shell.spotlight_query).len();
                        if count > 0 {
                            st.shell.spotlight_selected = (st.shell.spotlight_selected + 1).min(count - 1);
                        }
                        st.shell.overlay_dirty = true;
                    }
                    keysyms::KEY_Return => {
                        let results = st.app_registry.search(&st.shell.spotlight_query);
                        let sel = st.shell.spotlight_selected.min(results.len().saturating_sub(1));
                        if let Some(app) = results.get(sel) {
                            deferred = Some(KeyAction::Shell(ShellClickAction::LaunchApp(
                                app.action.clone(),
                                app.name.clone(),
                            )));
                        }
                        st.shell.spotlight_open = false;
                        st.shell.spotlight_query.clear();
                        st.shell.overlay_dirty = true;
                        st.shell.topbar_dirty = true;
                    }
                    raw => {
                        // Field keys: caret, Shift-selection, Ctrl+A, words, Backspace/Delete
                        let utf8 = sym.key_char().map(String::from);
                        let key = if mods.ctrl { latin_sym.raw() } else { raw };
                        let sh = &mut st.shell;
                        if sh.spotlight_input.text() != sh.spotlight_query {
                            sh.spotlight_input.set(&sh.spotlight_query);
                        }
                        match sh.spotlight_input.key(key, utf8.as_deref(), mods.ctrl, mods.shift) {
                            feather_ui::input::Edit::Changed => {
                                sh.spotlight_query = sh.spotlight_input.text().to_owned();
                                sh.spotlight_selected = 0;
                                sh.overlay_dirty = true;
                            }
                            feather_ui::input::Edit::Moved => sh.overlay_dirty = true,
                            feather_ui::input::Edit::Ignored => {}
                        }
                    }
                }
                return FilterResult::Intercept(());
            }

            // Built-in Notes window receives text when it is the focused (topmost) window
            if st.system_settings.editing()&&st.windows.iter().rev().find(|w|w.is_visible()).is_some_and(|w|matches!(w.backend,WindowBackend::Builtin(BuiltinWindowKind::SystemSettings))){
                if let Some(action)=st.system_settings.edit_key(if mods.ctrl{latin_sym.raw()}else{sym.raw()},sym.key_char(),mods.ctrl,mods.shift){deferred=Some(KeyAction::Shell(ShellClickAction::SystemSettings(action)));}
                st.mark_settings_dirty();st.needs_redraw=true;return FilterResult::Intercept(());
            }
            if let Some(top_win) = st.windows.iter_mut().rev().find(|w| w.is_visible()) {
                if let WindowBackend::Builtin(BuiltinWindowKind::QuickNotes) = top_win.backend {
                    match sym.raw() {
                        keysyms::KEY_BackSpace => {
                            st.notes_buffer.pop();
                            top_win.dirty = true;
                            return FilterResult::Intercept(());
                        }
                        keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                            st.notes_buffer.push('\n');
                            top_win.dirty = true;
                            return FilterResult::Intercept(());
                        }
                        _ => {
                            if let Some(ch) = sym.key_char() {
                                if !ch.is_control() {
                                    st.notes_buffer.push(ch);
                                    top_win.dirty = true;
                                    return FilterResult::Intercept(());
                                }
                            }
                        }
                    }
                }
            }

            FilterResult::Forward
        });

        match deferred {
            Some(KeyAction::Lock)=>self.lock_session(),
            Some(KeyAction::Unlock)=>self.lock_submit(),
            Some(KeyAction::Auth(cmd)) => self.auth_command(cmd),
            Some(KeyAction::SwitcherStep { reverse }) => self.switcher_step(reverse),
            Some(KeyAction::SwitcherCommit) => self.switcher_commit(),
            Some(KeyAction::SwitchVt(vt)) => self.switch_vt(vt),
            // Any shortcut while selecting (Esc too) ends the selection first
            Some(KeyAction::Screenshot { .. }) if self.screenshot_select.is_some() => self.cancel_screenshot_selection(),
            Some(KeyAction::Screenshot { shift: true, .. }) => self.start_screenshot_selection(),
            Some(KeyAction::Screenshot { alt: true, .. }) => match self.window_target(|w| w.activated) {
                Some(target) => self.take_screenshot(target),
                None => self.take_screenshot(crate::screenshot::Target::Screen),
            },
            Some(KeyAction::Screenshot { .. }) => self.take_screenshot(crate::screenshot::Target::Screen),
            Some(KeyAction::SwitcherCancel) => {
                self.shell.switcher = None;
            }
            Some(KeyAction::Shell(act)) => self.execute_shell_action(act),
            Some(KeyAction::CloseTop) => self.close_top_window(),
            Some(KeyAction::MinimizeTop) => {
                if let Some(top) = self.windows.iter_mut().rev().find(|w| w.is_visible()) {
                    top.minimized = true;
                    self.shell.dock_dirty = true;
                    self.refresh_focus();
                }
            }
            Some(KeyAction::ToggleMaximizeTop) => {
                let (sw, sh) = (self.shell.screen_w, self.shell.screen_h);
                if let Some(top) = self.windows.iter_mut().rev().find(|w| w.is_visible()) {
                    top.resize_anchor = None;
                    top.toggle_maximize(sw, sh);
                }
            }
            None => {}
        }
    }

    fn switcher_step(&mut self, reverse: bool) {
        if self.shell.switcher.is_some() {
            self.shell.switcher_step(if reverse { -1 } else { 1 });
            return;
        }
        let items = self.switcher_items();
        if items.is_empty() {
            return;
        }
        self.shell.close_menus();
        self.shell.open_switcher(items, reverse);
        let _ = self.loop_handle.insert_source(
            smithay::reexports::calloop::timer::Timer::from_duration(SWITCHER_SHOW_DELAY),
            |_, _, state| {
                if let Some(sw) = state.shell.switcher.as_mut() {
                    sw.visible = true;
                    state.shell.switcher_dirty = true;
                    state.needs_redraw = true;
                }
                smithay::reexports::calloop::timer::TimeoutAction::Drop
            },
        );
    }

    /// Whether committing the switcher would focus a different app than the current one
    fn switcher_changes_focus(&self) -> bool {
        let Some(sw) = self.shell.switcher.as_ref() else {
            return false;
        };
        let target = sw.items.get(sw.selected).map(|i| i.key);
        let current = self
            .windows
            .iter()
            .rev()
            .find(|w| w.is_visible())
            .map(|w| self.app_key(w));
        target.is_some() && target != current
    }

    fn switcher_commit(&mut self) {
        self.unfocus_desktop();
        if let Some(sw) = self.shell.switcher.take() {
            if let Some(item) = sw.items.get(sw.selected) {
                self.activate_app(item.key);
            }
        }
    }

    fn close_top_window(&mut self) {
        let Some(idx) = self.windows.iter().rposition(|w| w.is_visible()) else {
            return;
        };
        match self.windows[idx].backend {
            WindowBackend::Wayland(ref toplevel) => toplevel.send_close(),
            WindowBackend::Builtin(_) => {
                self.windows.remove(idx);
                self.refresh_focus();
            }
        }
        self.shell.topbar_dirty = true;
        self.shell.dock_dirty = true;
    }

    /// End-to-end tests inside our own nested compositor and test clients only
    /// (FEATHER_DEBUG_DND); the host never receives these events
    pub fn debug_pointer_motion(&mut self, x: f64, y: f64) {
        let time = self.start_time.elapsed().as_millis() as u32;
        self.needs_redraw = true;
        self.on_pointer_motion((x, y).into(), time);
    }

    pub fn debug_pointer_button(&mut self, button: u32, pressed: bool) {
        let time = self.start_time.elapsed().as_millis() as u32;
        self.needs_redraw = true;
        let state = if pressed { ButtonState::Pressed } else { ButtonState::Released };
        if pressed {
            self.pressed_buttons.push(button);
        }
        self.on_pointer_button(button, state, time);
    }

    fn set_compositor_cursor(&mut self, icon: CursorIcon) {
        if self.compositor_cursor != Some(icon) {
            self.compositor_cursor = Some(icon);
            self.apply_cursor(CursorImageStatus::Named(icon));
        }
    }

    fn on_pointer_motion(&mut self, pos: Point<f64, Logical>, time: u32) {
        self.pointer_location = pos;
        if self.lock.active {
            let under=self.surface_under_pointer(pos.x,pos.y);let pointer=self.pointer.clone();
            pointer.motion(self,under,&MotionEvent{location:pos,serial:SERIAL_COUNTER.next_serial(),time});pointer.frame(self);return;
        }

        if self.shell.switcher.as_ref().is_some_and(|s|s.visible){
            if let Some(index)=self.shell.switcher_hit(pos.x,pos.y){
                if let Some(sw)=self.shell.switcher.as_mut(){if sw.selected!=index{sw.selected=index;self.shell.switcher_dirty=true;self.needs_redraw=true;}}
            }
            self.pointer_focus_dirty=true;return;
        }
        if self.screenshot_motion() || self.desktop_motion() {
            return;
        }
        if self.shell.control_center_open && self.shell.cc.motion(pos.x as f32) {
            self.shell.overlay_dirty=true;
            return;
        }

        if let Some(drag) = self.active_drag {
            self.apply_drag(drag, pos);
            return;
        }
        if self.icon_drag.is_some() {
            self.update_icon_drag();
            if self.icon_drag.as_ref().is_some_and(|d| d.active) {
                return;
            }
        }

        self.shell.update_pointer_hover(pos.x, pos.y);
        for win in &mut self.windows {
            if win.is_visible() {
                win.update_hover(pos.x, pos.y);
            }
        }
        self.settings_pointer_moved(pos.x, pos.y);
        if self.auth.motion((pos.x, pos.y), (self.shell.screen_w, self.shell.screen_h)) {
            self.needs_redraw = true;
        }

        let under = self.surface_under_pointer(pos.x, pos.y);
        if under.is_some() {
            // The client sets its own cursor on enter
            self.compositor_cursor = None;
        } else {
            let edges = self
                .windows
                .iter()
                .rev()
                .filter(|w| w.is_visible())
                .find_map(|w| w.resize_edges_at(pos.x, pos.y));
            let icon = edges.map(cursor_for_edges).unwrap_or(CursorIcon::Default);
            self.set_compositor_cursor(icon);
        }

        let pointer = self.pointer.clone();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
    }

    fn apply_drag(&mut self, drag: DragOperation, pos: Point<f64, Logical>) {
        match drag {
            DragOperation::MoveWindow {
                win_id,
                start_cursor,
                start_pos,
            } => {
                let dx = (pos.x - start_cursor.0).round() as i32;
                let dy = (pos.y - start_cursor.1).round() as i32;
                if let Some(win) = self.windows.iter_mut().find(|w| w.id == win_id) {
                    win.x = start_pos.0 + dx;
                    win.y = (start_pos.1 + dy).max(TOP_BAR_HEIGHT);
                }
            }
            DragOperation::ResizeWindow {
                win_id,
                start_cursor,
                start_rect: (x0, y0, w0, h0),
                edges,
            } => {
                let dx = (pos.x - start_cursor.0).round() as i32;
                let dy = (pos.y - start_cursor.1).round() as i32;
                let mut w = w0;
                let mut h = h0;
                if edges.right {
                    w += dx;
                }
                if edges.left {
                    w -= dx;
                }
                if edges.bottom {
                    h += dy;
                }
                if edges.top {
                    h -= dy;
                }
                let w = w.max(MIN_WINDOW_W);
                let h = h.max(MIN_WINDOW_H);
                if let Some(win) = self.windows.iter_mut().find(|w| w.id == win_id) {
                    if win.is_builtin() {
                        if edges.left {
                            win.x = x0 + (w0 - w);
                        }
                        if edges.top {
                            win.y = (y0 + (h0 - h)).max(TOP_BAR_HEIGHT);
                        }
                    }
                    win.resize_client(w, h);
                }
            }
        }
    }

    /// The toplevel surface owning `surface` (through subsurfaces), and whether it is a popup
    fn owner_of_surface(&self, surface: &WlSurface) -> (Option<WlSurface>, bool) {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        if let Some(popup) = self.popups.find_popup(&root) {
            return (find_popup_root_surface(&popup).ok(), true);
        }
        (Some(root), false)
    }

    /// Emulates popup grabs: a click outside of a window's popups closes them
    fn dismiss_popups_except(&mut self, keep_for: Option<&WlSurface>) {
        for win in &self.windows {
            let Some(surface) = win.wl_surface() else { continue };
            if Some(surface) == keep_for {
                continue;
            }
            for (popup, _) in PopupManager::popups_for_surface(surface) {
                let _ = PopupManager::dismiss_popup(surface, &popup);
            }
        }
    }

    fn start_resize(&mut self, idx: usize, edges: Edges) {
        let pos = self.pointer_location;
        let win = &mut self.windows[idx];
        win.resize_anchor = Some(ResizeAnchor {
            edges,
            right: win.x + win.width,
            bottom: win.y + win.total_height(),
        });
        self.active_drag = Some(DragOperation::ResizeWindow {
            win_id: win.id,
            start_cursor: (pos.x, pos.y),
            start_rect: (win.x, win.y, win.width, win.height),
            edges,
        });
    }

    fn start_move(&mut self, idx: usize) {
        let pos = self.pointer_location;
        let win = &mut self.windows[idx];
        win.resize_anchor = None;
        self.active_drag = Some(DragOperation::MoveWindow {
            win_id: win.id,
            start_cursor: (pos.x, pos.y),
            start_pos: (win.x, win.y),
        });
    }

    fn on_pointer_button(&mut self, button: u32, btn_state: ButtonState, time: u32) {
        if self.lock.active {
            if self.lock.external() {
                if btn_state==ButtonState::Pressed {if !self.pressed_buttons.contains(&button){self.pressed_buttons.push(button);}}
                else {self.pressed_buttons.retain(|held|*held!=button);}
                let pointer=self.pointer.clone();
                pointer.button(self,&ButtonEvent{button,state:btn_state,serial:SERIAL_COUNTER.next_serial(),time});pointer.frame(self);
            } else if button==BTN_LEFT && btn_state==ButtonState::Pressed {
                let x=self.pointer_location.x-(self.shell.screen_w as f64-400.0)/2.0;
                let y=self.pointer_location.y-(self.shell.screen_h as f64-260.0)/2.0;
                if (34.0..366.0).contains(&x) && (119.0..157.0).contains(&y) {
                    self.lock.click_field(&mut self.fonts,x as f32,self.keyboard.modifier_state().shift);self.needs_redraw=true;
                }
                if (115.0..285.0).contains(&x) && (204.0..236.0).contains(&y) {self.lock_submit();}
            }
            return;
        }

        // A release of a press that started before the selection still belongs to the client
        let earlier_press = btn_state == ButtonState::Released && self.pressed_buttons.contains(&button);
        if !earlier_press && self.screenshot_button(button == BTN_LEFT, btn_state == ButtonState::Pressed) {
            return;
        }
        let serial = SERIAL_COUNTER.next_serial();
        let pos = self.pointer_location;
        let pointer = self.pointer.clone();
        let event = ButtonEvent {
            button,
            state: btn_state,
            serial,
            time,
        };

        // Modal password dialog: every press is swallowed; its release is consumed below.
        if btn_state == ButtonState::Pressed && self.auth.is_open() {
            if button == BTN_LEFT {
                if let Some(cmd) = self.auth.click((pos.x, pos.y), (self.shell.screen_w, self.shell.screen_h)) {
                    self.auth_command(cmd);
                }
            }
            self.consumed_buttons.push(button);
            self.needs_redraw = true;
            return;
        }
        if btn_state == ButtonState::Released {
            if button==BTN_LEFT {
                if let Some(action)=self.shell.cc.release() {self.shell.overlay_dirty=true;self.execute_shell_action(action);}
                self.desktop_release();
            }
            self.pressed_buttons.retain(|&b| b != button);
            let was_dragging = self.active_drag.take().is_some();
            // Don't send a release for a press the client never saw
            if let Some(i) = self.consumed_buttons.iter().position(|&b| b == button) {
                self.consumed_buttons.swap_remove(i);
            } else {
                pointer.button(self, &event);
                pointer.frame(self);
            }
            if button == BTN_LEFT && self.icon_drag.is_some() {
                self.finish_icon_press();
            }
            if was_dragging || self.pressed_buttons.is_empty() {
                // Re-enter the surface now under the cursor with fresh coordinates
                self.pointer_focus_dirty = true;
            }
            return;
        }
        // A release we never got (e.g. the host stole the pointer) must not leave stale state
        // behind, otherwise the *next* release would be swallowed and the client stays grabbed.
        self.consumed_buttons.retain(|&b| b != button);
        if !self.pressed_buttons.contains(&button) {
            self.pressed_buttons.push(button);
        }
        self.user_interacted = true;
        if self.shell.switcher.as_ref().is_some_and(|s|s.visible){
            self.consumed_buttons.push(button);
            if button==BTN_LEFT {
                if let Some(index)=self.shell.switcher_hit(pos.x,pos.y){
                    self.shell.switcher.as_mut().unwrap().selected=index;
                    self.switcher_commit();return;
                }
            }
            self.shell.switcher=None;self.pointer_focus_dirty=true;self.needs_redraw=true;return;
        }
        if button!=BTN_LEFT && self.shell.control_center_open {
            self.consumed_buttons.push(button); return;
        }
        if self.pressed_buttons.len() == 1 {
            // Make sure the press goes to the surface really under the cursor right now
            self.refresh_pointer_focus();
        }

        let fullscreen_top = self
            .windows
            .iter()
            .rev()
            .find(|w| w.is_visible())
            .is_some_and(|w| w.fullscreen);
        if (!fullscreen_top || self.shell.has_visible_overlay()) && self.shell.launchpad.is_none() {
            if let Some((item,_))=self.shell.tray_hit(pos.x,pos.y).map(|(i,r)|(i.clone(),r)) {
                let action=match button {
                    BTN_LEFT if !item.menu_only=>crate::tray::Action::Activate,
                    0x112=>crate::tray::Action::Secondary,
                    _=>crate::tray::Action::Context,
                };
                self.consumed_buttons.push(button);
                self.execute_shell_action(ShellClickAction::Tray{key:item.key,action,x:pos.x as i32,y:pos.y as i32});
                return;
            }
        }
        // Middle click on a Dock app: another copy of it
        if button == 0x112 && self.shell.dock_menu.is_none() {
            if let Some((DockTarget::App(i), _)) = self.shell.dock_target_at(pos.x, pos.y) {
                self.consumed_buttons.push(button);
                self.launch_app_instance(i);
                return;
            }
        }
        // Right click on a Dock icon: its context menu
        if button == BTN_RIGHT && self.shell.dock_menu.is_none() {
            if let Some((target, anchor)) = self.shell.dock_target_at(pos.x, pos.y) {
                self.consumed_buttons.push(button);
                self.open_dock_menu_for(target, anchor);
                return;
            }
            if self.shell.dock_contains(pos.x, pos.y) {
                self.consumed_buttons.push(button);
                self.open_dock_background_menu();
                return;
            }
        }

        // Launchpad covers the desktop; the menu bar and the Dock stay usable above it
        // Launchpad hides the menu bar; only the Dock stays usable above it
        let over_chrome = self.shell.dock_target_at(pos.x, pos.y).is_some();
        if self.shell.launchpad.is_some() && !over_chrome && self.shell.dock_menu.is_none() {
            self.consumed_buttons.push(button);
            let hit = self.shell.launchpad.as_ref().map(|l| l.hit(pos.x as f32, pos.y as f32));
            match hit {
                Some(LaunchpadHit::App(app)) if button == BTN_LEFT => {
                    let entry = &self.app_registry.apps[app];
                    let click = ShellClickAction::LaunchpadLaunch(app);
                    let (icon, label) = (AppIcon::of(entry), entry.name.clone());
                    self.begin_icon_press(DragSource::Launchpad { app }, icon, label, click);
                }
                Some(LaunchpadHit::Dot(page)) => {
                    if let Some(l) = self.shell.launchpad.as_mut() {
                        l.set_page(page);
                    }
                }
                Some(LaunchpadHit::Search) => {
                    let shift = self.keyboard.modifier_state().shift;
                    if let Some(l) = self.shell.launchpad.as_mut() {
                        l.focus_search(true);
                        l.click_search(&mut self.fonts, pos.x as f32, shift);
                    }
                }
                _ => {
                    tracing::debug!("Launchpad закрыт кликом по фону");
                    self.shell.launchpad = None
                }
            }
            return;
        }

        if button == BTN_LEFT && self.shell.control_center_open && self.shell.cc.page == crate::control_panel::Page::Password {
            let panel = self.shell.control_panel_rect();
            self.shell.cc.click_password(panel, pos.x as f32, pos.y as f32, &mut self.fonts, self.keyboard.modifier_state().shift);
        }
        if !fullscreen_top || self.shell.has_visible_overlay() {
            if let Some(action) = self
                .shell
                .handle_pointer_click(pos.x, pos.y, &self.app_registry)
            {
                self.consumed_buttons.push(button);
                self.dismiss_popups_except(None);
                if matches!(action, ShellClickAction::DockPress(_)) {
                    self.unfocus_desktop();
                }
                if let ShellClickAction::DockPress(target) = action {
                    if button == BTN_LEFT {
                        self.press_dock_icon(target);
                    }
                    return;
                }
                self.execute_shell_action(action);
                return;
            }
        }

        let under = self.surface_under_pointer(pos.x, pos.y);
        let (owner, on_popup) = under
            .as_ref()
            .map(|(s, _)| self.owner_of_surface(s))
            .unwrap_or((None, false));
        self.dismiss_popups_except(if on_popup { owner.as_ref() } else { None });

        let hit_index = match owner {
            Some(ref owner) => self.window_index_for_toplevel(owner),
            None => self.windows.iter().rposition(|w| {
                w.is_visible() && (w.contains_point(pos.x, pos.y) || w.resize_edges_at(pos.x, pos.y).is_some())
            }),
        };

        let Some(idx) = hit_index else {
            // Click on the bare desktop: icons, rubber band, keyboard focus
            let mods = self.keyboard.modifier_state();
            if self.desktop_press(button == BTN_LEFT, mods.ctrl, mods.shift) {
                self.consumed_buttons.push(button);
                return;
            }
            pointer.button(self, &event);
            pointer.frame(self);
            return;
        };

        // Raise and focus on every click: also repairs focus after a window was closed
        self.unfocus_desktop();
        let win = self.windows.remove(idx);
        self.windows.push(win);
        self.refresh_focus();
        let idx = self.windows.len() - 1;

        let super_held = self.keyboard.modifier_state().logo;
        let mut consumed = true;
        let mut builtin_action: Option<ShellClickAction> = None;
        let (sw, sh) = (self.shell.screen_w, self.shell.screen_h);

        if super_held && button == BTN_LEFT {
            self.start_move(idx);
        } else if super_held && button == BTN_RIGHT {
            self.start_resize(idx, Edges::BOTTOM_RIGHT);
        } else if let Some(edges) = self.windows[idx].resize_edges_at(pos.x, pos.y) {
            self.start_resize(idx, edges);
        } else if let Some(btn) = self.windows[idx].hit_test_traffic(pos.x, pos.y) {
            match btn {
                crate::window::TrafficButton::Close => self.close_top_window(),
                crate::window::TrafficButton::Minimize => {
                    self.windows[idx].minimized = true;
                    self.shell.dock_dirty = true;
                    self.refresh_focus();
                }
                crate::window::TrafficButton::Maximize => {
                    self.windows[idx].resize_anchor = None;
                    self.windows[idx].toggle_maximize(sw, sh);
                }
            }
        } else if self.windows[idx].in_titlebar(pos.x, pos.y) && button == BTN_LEFT {
            let win_id = self.windows[idx].id;
            let now = Instant::now();
            let is_double = self
                .last_titlebar_click
                .is_some_and(|(t, id)| id == win_id && now.duration_since(t).as_millis() < DOUBLE_CLICK_MS);
            if is_double {
                self.last_titlebar_click = None;
                self.windows[idx].resize_anchor = None;
                self.windows[idx].toggle_maximize(sw, sh);
            } else {
                self.last_titlebar_click = Some((now, win_id));
                self.start_move(idx);
            }
        } else if let WindowBackend::Builtin(kind) = self.windows[idx].backend {
            let win = &self.windows[idx];
            let lx = (pos.x as f32) - (win.x as f32);
            let ly = (pos.y as f32) - ((win.y + TITLEBAR_HEIGHT) as f32);
            match kind {
                BuiltinWindowKind::AboutFeather => {
                    if ly >= win.height as f32 - 50.0 {
                        builtin_action = Some(if lx < win.width as f32 - 175.0 {
                            ShellClickAction::ToggleShowAbout
                        } else {
                            ShellClickAction::CycleTheme
                        });
                    }
                }
                BuiltinWindowKind::SystemSettings => {
                    let (width, height) = (win.width as f32, win.height as f32);
                    let action = self.system_settings.hit(lx, ly, width, height, &self.settings_context());
                    if let Some(action) = &action {
                        self.settings_text_click(action, lx, ly, width, height);
                    }
                    builtin_action = action.map(ShellClickAction::SystemSettings);
                }
                _ => {}
            }
        } else {
            consumed = false;
        }

        if consumed {
            self.consumed_buttons.push(button);
            if let Some(act) = builtin_action {
                self.execute_shell_action(act);
            }
            return;
        }

        pointer.button(self, &event);
        pointer.frame(self);
    }

    fn press_dock_icon(&mut self, target: DockTarget) {
        let (source, click, icon, label) = match target {
            DockTarget::App(i) => {
                let app = &self.app_registry.apps[i];
                (
                    DragSource::DockApp {
                        id: app.id.clone(),
                        pinned: self.is_pinned(i),
                    },
                    ShellClickAction::DockPress(target),
                    AppIcon::of(app),
                    app.name.clone(),
                )
            }
            DockTarget::Window(_) => (
                DragSource::DockOther,
                ShellClickAction::DockPress(target),
                AppIcon {
                    kind: crate::apps::IconKind::GenericApp,
                    name: None,
                },
                String::new(),
            ),
            DockTarget::Minimized(_) => (
                DragSource::DockOther,
                ShellClickAction::DockPress(target),
                AppIcon {
                    kind: crate::apps::IconKind::GenericApp,
                    name: None,
                },
                String::new(),
            ),
            DockTarget::Trash => (
                DragSource::DockOther,
                ShellClickAction::DockPress(target),
                AppIcon {
                    kind: crate::apps::IconKind::TrashEmpty,
                    name: None,
                },
                "Корзина".into(),
            ),
        };
        self.begin_icon_press(source, icon, label, click);
    }

    /// Re-sends pointer focus for the current location (after stacking/geometry changes)
    pub fn refresh_pointer_focus(&mut self) {
        self.pointer_focus_dirty = false;
        if self.active_drag.is_some() || self.pointer.is_grabbed() {
            return;
        }
        let pos = self.pointer_location;
        let under = self.surface_under_pointer(pos.x, pos.y);
        let pointer = self.pointer.clone();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time: self.start_time.elapsed().as_millis() as u32,
            },
        );
        pointer.frame(self);
    }

    /// The host window lost focus: whatever is held now will be released elsewhere and we
    /// would never hear about it (stuck Alt, stuck mouse button = dead clicks)
    pub fn release_all_input(&mut self) {
        let time = self.start_time.elapsed().as_millis() as u32;
        let pointer = self.pointer.clone();
        let client_held: Vec<u32> = self
            .pressed_buttons
            .iter()
            .copied()
            .filter(|b| !self.consumed_buttons.contains(b))
            .collect();
        for button in client_held {
            pointer.button(
                self,
                &ButtonEvent {
                    button,
                    state: ButtonState::Released,
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
        }
        pointer.frame(self);
        self.pressed_buttons.clear();
        self.consumed_buttons.clear();
        self.active_drag = None;

        let keyboard = self.keyboard.clone();
        for key in keyboard.pressed_keys() {
            keyboard.input::<(), _>(
                self,
                key,
                KeyState::Released,
                SERIAL_COUNTER.next_serial(),
                time,
                |_, _, _| FilterResult::Forward,
            );
        }
        self.pointer_focus_dirty = true;
    }

    fn on_pointer_axis<B: InputBackend>(&mut self, event: B::PointerAxisEvent) {
        if self.lock.active {
            if self.lock.external(){self.forward_pointer_axis::<B>(event);}
            return;
        }
        let fullscreen=self.windows.iter().rev().find(|w|w.is_visible()).is_some_and(|w|w.fullscreen);
        if !fullscreen&&self.shell.launchpad.is_none() {
            if let Some(key)=self.shell.tray_hit(self.pointer_location.x,self.pointer_location.y).map(|(i,_)|i.key.clone()) {
                for (axis,horizontal) in [(Axis::Vertical,false),(Axis::Horizontal,true)] {
                    let delta=event.amount_v120(axis).unwrap_or_else(||event.amount(axis).unwrap_or(0.0)*8.0).round() as i32;
                    if delta!=0 {self.execute_shell_action(ShellClickAction::Tray{key:key.clone(),action:crate::tray::Action::Scroll(-delta,horizontal),x:self.pointer_location.x as i32,y:self.pointer_location.y as i32});}
                }
                return;
            }
        }
        let menu_delta = event.amount(Axis::Vertical).unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0));
        if self.shell.control_center_open && self.shell.cc.page!=crate::control_panel::Page::Main {
            self.shell.cc.scroll(menu_delta); self.shell.overlay_dirty=true; return;
        }
        if self.shell.scroll_client_menu(self.pointer_location.x as f32, self.pointer_location.y as f32, menu_delta) {
            self.needs_redraw |= self.shell.overlay_dirty;
            return;
        }
        if let Some(lp) = self.shell.launchpad.as_mut() {
            // Wheel notch = one page; touchpad swipe = one page per gesture
            let discrete_v = event.amount_v120(Axis::Vertical).filter(|v| *v != 0.0);
            let discrete_h = event.amount_v120(Axis::Horizontal).filter(|v| *v != 0.0);
            let (delta, discrete) = match (discrete_v, discrete_h) {
                (Some(v), _) => (v.signum(), true),
                (None, Some(h)) => (h.signum(), true),
                _ => {
                    let v = event.amount(Axis::Vertical).unwrap_or(0.0);
                    let h = event.amount(Axis::Horizontal).unwrap_or(0.0);
                    (if h.abs() > v.abs() { h } else { v }, event.source() == AxisSource::Wheel)
                }
            };
            if delta != 0.0 && lp.scroll(delta, discrete) {
                self.needs_redraw = true;
            }
            return;
        }
        if let Some((w,h))=self.windows.iter().rev().find(|w|w.contains_point(self.pointer_location.x,self.pointer_location.y)).filter(|w|matches!(w.backend,WindowBackend::Builtin(BuiltinWindowKind::SystemSettings))).map(|w|(w.width as f32,w.height as f32)){
            let mut panel=std::mem::take(&mut self.system_settings);
            let changed=panel.scroll_by(menu_delta,w,h,&self.settings_context());
            self.system_settings=panel;
            if changed{self.mark_settings_dirty();self.needs_redraw=true;}
            return;
        }
        self.forward_pointer_axis::<B>(event);
    }
    fn forward_pointer_axis<B: InputBackend>(&mut self,event:B::PointerAxisEvent){
        let source = event.source();
        let horizontal_amount = event
            .amount(Axis::Horizontal)
            .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.0);
        let vertical_amount = event
            .amount(Axis::Vertical)
            .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.0);
        let mut frame = AxisFrame::new(Event::time_msec(&event)).source(source);
        if horizontal_amount != 0.0 {
            frame = frame.value(Axis::Horizontal, horizontal_amount);
            if let Some(v120) = event.amount_v120(Axis::Horizontal) {
                frame = frame.v120(Axis::Horizontal, v120 as i32);
            }
        }
        if vertical_amount != 0.0 {
            frame = frame.value(Axis::Vertical, vertical_amount);
            if let Some(v120) = event.amount_v120(Axis::Vertical) {
                frame = frame.v120(Axis::Vertical, v120 as i32);
            }
        }
        if source == AxisSource::Finger {
            if event.amount(Axis::Horizontal) == Some(0.0) {
                frame = frame.stop(Axis::Horizontal);
            }
            if event.amount(Axis::Vertical) == Some(0.0) {
                frame = frame.stop(Axis::Vertical);
            }
        }
        let pointer = self.pointer.clone();
        pointer.axis(self, frame);
        pointer.frame(self);
    }
}
