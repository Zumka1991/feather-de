//! Lists of open windows for other programs: the read-only ext-foreign-toplevel-list and
//! wlr-foreign-toplevel-management (taskbars such as waybar's, docks, rofi's window mode), which
//! can also activate, close and resize them. Updates come from focus changes and title commits;
//! wlr handles exist only for clients that bound the manager.
use std::collections::HashMap;

use smithay::{
    delegate_foreign_toplevel_list,
    reexports::{
        wayland_protocols_wlr::foreign_toplevel::v1::server::{
            zwlr_foreign_toplevel_handle_v1::{self, State as WlrState, ZwlrForeignToplevelHandleV1},
            zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
        },
        wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource},
    },
    wayland::foreign_toplevel_list::{ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState},
};

use crate::shell::ShellClickAction;
use crate::FeatherCompositor;

/// What a client was last told about a window
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Published {
    title: String,
    app_id: String,
    activated: bool,
    minimized: bool,
    maximized: bool,
    fullscreen: bool,
}

#[derive(Default)]
pub struct ForeignToplevels {
    managers: Vec<ZwlrForeignToplevelManagerV1>,
    /// Per window: its wlr handles and the ext handle
    wlr: HashMap<u64, (Vec<ZwlrForeignToplevelHandleV1>, Published)>,
    ext: HashMap<u64, ForeignToplevelHandle>,
}

impl ForeignToplevelListHandler for FeatherCompositor {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.foreign_list_state
    }
}
delegate_foreign_toplevel_list!(FeatherCompositor);

fn state_bytes(p: &Published) -> Vec<u8> {
    let mut states = Vec::new();
    for (on, state) in [(p.activated, WlrState::Activated), (p.minimized, WlrState::Minimized), (p.maximized, WlrState::Maximized), (p.fullscreen, WlrState::Fullscreen)] {
        if on {
            states.extend_from_slice(&(state as u32).to_ne_bytes());
        }
    }
    states
}

fn send_wlr(handle: &ZwlrForeignToplevelHandleV1, before: Option<&Published>, now: &Published) {
    if before.is_none_or(|b| b.title != now.title) {
        handle.title(now.title.clone());
    }
    if before.is_none_or(|b| b.app_id != now.app_id) {
        handle.app_id(now.app_id.clone());
    }
    handle.state(state_bytes(now));
    handle.done();
}

fn published(win: &crate::window::ManagedWindow) -> Published {
    Published { title: win.title.clone(), app_id: win.app_id.clone(), activated: win.activated, minimized: win.minimized, maximized: win.maximized, fullscreen: win.fullscreen }
}

impl FeatherCompositor {
    /// After any change of the window list or of a window's state
    pub fn foreign_sync(&mut self) {
        self.sync_ext_list();
        if self.foreign.managers.is_empty() {
            return;
        }
        let listed: Vec<(u64, Published)> = self.windows.iter().filter(|w| w.mapped && !w.larvis).map(|w| (w.id, published(w))).collect();
        let dh = self.display_handle.clone();
        let managers = self.foreign.managers.clone();
        self.foreign.wlr.retain(|id, (handles, _)| {
            let open = listed.iter().any(|(l, _)| l == id);
            if !open {
                for handle in handles.iter() {
                    handle.closed();
                }
            }
            open
        });
        for (id, now) in listed {
            match self.foreign.wlr.get_mut(&id) {
                Some((handles, before)) if *before != now => {
                    handles.retain(|h| h.is_alive());
                    for handle in handles.iter() {
                        send_wlr(handle, Some(before), &now);
                    }
                    *before = now;
                }
                Some(_) => {}
                None => {
                    let handles = managers.iter().filter_map(|m| announce(&dh, m, id, &now)).collect();
                    self.foreign.wlr.insert(id, (handles, now));
                }
            }
        }
    }

    fn sync_ext_list(&mut self) {
        let windows: Vec<(u64, String, String)> = self.windows.iter().filter(|w| w.mapped && !w.larvis).map(|w| (w.id, w.title.clone(), w.app_id.clone())).collect();
        let list = &mut self.foreign_list_state;
        self.foreign.ext.retain(|id, handle| {
            let open = windows.iter().any(|(w, ..)| w == id);
            if !open {
                list.remove_toplevel(handle);
            }
            open
        });
        for (id, title, app_id) in windows {
            match self.foreign.ext.get(&id) {
                Some(handle) => {
                    if handle.title() != title || handle.app_id() != app_id {
                        handle.send_title(&title);
                        handle.send_app_id(&app_id);
                        handle.send_done();
                    }
                }
                None => {
                    let handle = list.new_toplevel::<FeatherCompositor>(title, app_id);
                    self.foreign.ext.insert(id, handle);
                }
            }
        }
    }

    fn foreign_request(&mut self, id: u64, request: zwlr_foreign_toplevel_handle_v1::Request) {
        use zwlr_foreign_toplevel_handle_v1::Request as R;
        let Some(idx) = self.windows.iter().position(|w| w.id == id) else { return };
        let r = self.window_monitor(idx);
        match request {
            R::Activate { .. } => self.execute_shell_action(ShellClickAction::FocusWindow(id)),
            R::Close => match &self.windows[idx].backend {
                crate::window::WindowBackend::Wayland(toplevel) => toplevel.send_close(),
                crate::window::WindowBackend::Builtin(_) => {
                    self.windows.remove(idx);
                    self.refresh_focus();
                }
            },
            R::SetMaximized => self.windows[idx].maximize_on(true, r),
            R::UnsetMaximized => self.windows[idx].maximize_on(false, r),
            R::SetFullscreen { .. } => self.set_window_fullscreen(idx, true, r),
            R::UnsetFullscreen => self.set_window_fullscreen(idx, false, r),
            R::SetMinimized => {
                self.windows[idx].minimized = true;
                self.shell.dock_dirty = true;
                self.refresh_focus();
            }
            R::UnsetMinimized => self.execute_shell_action(ShellClickAction::RestoreMinimizedWindow(id)),
            _ => {}
        }
        self.needs_redraw = true;
        self.foreign_sync();
    }
}

/// A new handle on one client's manager
fn announce(dh: &DisplayHandle, manager: &ZwlrForeignToplevelManagerV1, id: u64, now: &Published) -> Option<ZwlrForeignToplevelHandleV1> {
    let client = manager.client()?;
    let handle = client.create_resource::<ZwlrForeignToplevelHandleV1, _, FeatherCompositor>(dh, manager.version(), id).ok()?;
    manager.toplevel(&handle);
    send_wlr(&handle, None, now);
    Some(handle)
}

impl GlobalDispatch<ZwlrForeignToplevelManagerV1, ()> for FeatherCompositor {
    fn bind(state: &mut Self, dh: &DisplayHandle, _client: &Client, resource: New<ZwlrForeignToplevelManagerV1>, _: &(), init: &mut DataInit<'_, Self>) {
        let manager = init.init(resource, ());
        let existing: Vec<(u64, Published)> = state.windows.iter().filter(|w| w.mapped && !w.larvis).map(|w| (w.id, published(w))).collect();
        for (id, now) in existing {
            if let Some(handle) = announce(dh, &manager, id, &now) {
                state.foreign.wlr.entry(id).or_insert_with(|| (Vec::new(), now)).0.push(handle);
            }
        }
        state.foreign.managers.push(manager);
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for FeatherCompositor {
    fn request(state: &mut Self, _: &Client, manager: &ZwlrForeignToplevelManagerV1, request: zwlr_foreign_toplevel_manager_v1::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let zwlr_foreign_toplevel_manager_v1::Request::Stop = request {
            manager.finished();
            state.foreign.managers.retain(|m| m != manager);
        }
    }
    fn destroyed(state: &mut Self, _: smithay::reexports::wayland_server::backend::ClientId, manager: &ZwlrForeignToplevelManagerV1, _: &()) {
        state.foreign.managers.retain(|m| m != manager);
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, u64> for FeatherCompositor {
    fn request(state: &mut Self, _: &Client, _: &ZwlrForeignToplevelHandleV1, request: zwlr_foreign_toplevel_handle_v1::Request, id: &u64, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        state.foreign_request(*id, request);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_are_a_little_endian_array_of_what_is_on() {
        let p = Published { activated: true, maximized: true, ..Default::default() };
        let bytes = state_bytes(&p);
        assert_eq!(bytes.len(), 8);
        assert_eq!(u32::from_ne_bytes(bytes[0..4].try_into().unwrap()), WlrState::Activated as u32);
        assert_eq!(u32::from_ne_bytes(bytes[4..8].try_into().unwrap()), WlrState::Maximized as u32);
        assert!(state_bytes(&Published::default()).is_empty());
    }
}
