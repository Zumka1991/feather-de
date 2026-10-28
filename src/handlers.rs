use std::os::unix::io::OwnedFd;

use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        renderer::{utils::on_commit_buffer_handler, ImportDma},
    },
    delegate_compositor, delegate_cursor_shape, delegate_data_device, delegate_dmabuf,
    delegate_drm_syncobj, delegate_fractional_scale, delegate_output, delegate_primary_selection,
    delegate_seat, delegate_shm, delegate_viewporter, delegate_xdg_activation,
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{find_popup_root_surface, get_popup_toplevel_coords, PopupKind},
    input::{pointer::CursorImageStatus, Seat, SeatHandler, SeatState},
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
            shell::server::xdg_toplevel,
        },
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_output, wl_seat, wl_surface::WlSurface},
            Client, Resource,
        },
    },
    utils::{Logical, Rectangle, Serial, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            add_blocker, add_pre_commit_hook, get_parent, with_states, CompositorClientState,
            CompositorHandler, CompositorState,
        },
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        drm_syncobj::{DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState},
        fractional_scale::{self, FractionalScaleHandler},
        output::OutputHandler,
        selection::{
            data_device::{ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler},
            primary_selection::{PrimarySelectionHandler, PrimarySelectionState},
            SelectionHandler,
        },
        shell::xdg::{
            decoration::XdgDecorationHandler, PopupSurface, PositionerState, ToplevelSurface,
            XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
        tablet_manager::TabletSeatHandler,
        xdg_activation::{XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData},
    },
};

use crate::{
    theme::TOP_BAR_HEIGHT,
    window::{Edges, ManagedWindow, ResizeAnchor},
    DragOperation, FeatherCompositor,
};

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    pub xwayland: std::sync::atomic::AtomicBool,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl FeatherCompositor {
    pub fn window_index_for_toplevel(&self, surface: &WlSurface) -> Option<usize> {
        self.windows
            .iter()
            .position(|w| w.wl_surface() == Some(surface))
    }

    fn window_for_toplevel_mut(&mut self, surface: &WlSurface) -> Option<&mut ManagedWindow> {
        self.windows
            .iter_mut()
            .find(|w| w.wl_surface() == Some(surface))
    }

    /// Picks a position for a freshly mapped window and keeps it on screen
    fn place_new_window(&mut self, idx: usize) {
        let r=self.monitor_at(self.pointer_location.x,self.pointer_location.y);
        let sw=r.w;let sh=r.h;
        let cascade = (self.windows.iter().filter(|w| w.is_visible()).count() as i32 % 6) * 26;
        let win = &mut self.windows[idx];

        // Never let a client open bigger than the usable desktop
        let max_w = sw - 16;
        let max_h = sh - TOP_BAR_HEIGHT - crate::theme::dock_reserved() - win.titlebar_h();
        if (win.width > max_w || win.height > max_h) && !win.maximized && !win.fullscreen {
            let w = win.width.min(max_w);
            let h = win.height.min(max_h);
            win.width = w;
            win.height = h;
            win.resize_client(w, h);
        }
        if win.maximized || win.fullscreen {
            return;
        }
        // Cascade, but never push the window past the screen edges
        let min_y = TOP_BAR_HEIGHT + 8;
        let max_x = (sw - win.width).max(0);
        let max_y = (sh - win.total_height()).max(min_y);
        win.x = ((sw - win.width) / 2 + cascade).clamp(0, max_x);
        win.y = ((sh - win.total_height()) / 2 + cascade - 20).clamp(min_y, max_y);
        win.x+=r.x;win.y+=r.y;
    }

    /// Preferred scale of a window's surface, which for X windows depends on fullscreen.
    /// Satellite converts pointer coordinates with the scale it had at wl_pointer.enter, so a
    /// hovered surface whose scale changes gets the pointer again: otherwise clicks land at
    /// half (or double) X coordinates.
    pub fn sync_window_scale(&mut self, idx: usize) {
        let win = &self.windows[idx];
        let Some(surface) = win.wl_surface().cloned() else { return };
        let scale = crate::xwayland::preferred_scale(crate::xwayland::is_xwayland_surface(&surface), self.xwayland_hidpi, win.fullscreen, self.scale_factor);
        let changed = with_states(&surface, |states| {
            fractional_scale::with_fractional_scale(states, |fs| {
                let changed = fs.preferred_scale() != Some(scale);
                fs.set_preferred_scale(scale);
                changed
            })
        });
        if changed {
            self.reenter_pointer_if_on(&surface);
        }
    }

    /// Leave now, enter again with the next focus refresh (after a surface's scale changed)
    pub fn reenter_pointer_if_on(&mut self, surface: &WlSurface) {
        let pointer = self.pointer.clone();
        if pointer.current_focus().as_ref() != Some(surface) || pointer.is_grabbed() || self.pointer_locked() {
            return;
        }
        let location = self.pointer_location;
        let time = self.start_time.elapsed().as_millis() as u32;
        pointer.motion(self, None, &smithay::input::pointer::MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
        self.pointer_focus_dirty = true;
    }

    /// Fullscreen on/off on monitor `r`; the scale goes first so satellite sizes the X
    /// window for it when the configure arrives
    pub fn set_window_fullscreen(&mut self, idx: usize, on: bool, r: crate::monitors::Rect) {
        let was = self.windows[idx].fullscreen;
        self.windows[idx].fullscreen = on;
        self.sync_window_scale(idx);
        self.windows[idx].fullscreen = was;
        self.windows[idx].fullscreen_on(on, r);
        if let Some(t) = self.windows[idx].toplevel() {
            if t.is_initial_configure_sent() {
                t.send_pending_configure();
            }
        }
    }

    fn handle_toplevel_commit(&mut self, surface: &WlSurface) {
        let Some(idx) = self.window_index_for_toplevel(surface) else {
            return;
        };
        let changes = {
            let win = &mut self.windows[idx];
            if let Some(toplevel) = win.toplevel() {
                if !toplevel.is_initial_configure_sent() {
                    toplevel.send_configure();
                    return;
                }
            }
            win.sync_wayland_metadata()
        };

        if changes.newly_mapped || changes.unmapped {
            tracing::debug!(id = self.windows[idx].id, mapped = self.windows[idx].mapped, "window mapping changed");
            self.pointer_focus_dirty = true;
        }
        if changes.newly_mapped {
            // A new window opens on the desktop in front
            self.windows[idx].space = self.spaces.current;
            self.windows[idx].off_space = false;
            self.place_new_window(idx);
            // New windows go to the top of the stack and get keyboard focus
            let win = self.windows.remove(idx);
            self.windows.push(win);
            self.refresh_focus();
        }
        if changes.unmapped {
            self.drop_keyboard_focus_if(surface);
            self.refresh_focus();
        }
        if changes.title_changed || changes.newly_mapped || changes.unmapped {
            self.shell.topbar_dirty = true;
            self.dock_model_dirty = true;
        }
        if changes.title_changed || changes.newly_mapped {
            if let Some(i) = self.window_index_for_toplevel(surface) {
                self.sync_window_scale(i);
            }
        }
    }

    /// Electron/Chromium recreate the xdg_toplevel on the same wl_surface when a window is
    /// hidden and shown again. Smithay would treat re-focusing that surface as "no change" and
    /// never send a new wl_keyboard.enter, leaving the app unable to receive typing.
    fn drop_keyboard_focus_if(&mut self, surface: &WlSurface) {
        let keyboard = self.keyboard.clone();
        if keyboard.current_focus().as_ref() == Some(surface) {
            self.refresh_focus();
        }
    }

    fn unconstrain_popup(&self, popup: &PopupSurface, positioner: &PositionerState) -> Rectangle<i32, Logical> {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else {
            return positioner.get_geometry();
        };
        let Some(win) = self.windows.iter().find(|w| w.wl_surface() == Some(&root)) else {
            return positioner.get_geometry();
        };
        let (cx, cy) = win.client_origin();
        let parent_offset = get_popup_toplevel_coords(&kind);
        let index=self.windows.iter().position(|w|w.id==win.id).unwrap();let r=self.window_monitor(index);
        let top=if r.x==0&&r.y==0{TOP_BAR_HEIGHT}else{0};
        let mut target=Rectangle::<i32,Logical>::new((r.x,r.y+top).into(),(r.w,r.h-top).into());
        target.loc.x -= cx + parent_offset.x;
        target.loc.y -= cy + parent_offset.y;
        positioner.get_unconstrained_geometry(target)
    }

    fn begin_client_drag(&mut self, surface: &ToplevelSurface, edges: Option<Edges>) {
        if self.lock.active {return;}
        if self.pressed_buttons.is_empty() {
            return;
        }
        let pos = self.pointer_location;
        let Some(idx) = self.window_index_for_toplevel(surface.wl_surface()) else {
            return;
        };
        if self.windows[idx].fullscreen {
            return;
        }
        let win_id = self.windows[idx].id;
        let op = match edges {
            None => {
                if self.windows[idx].maximized {
                    // Dragging a maximized window restores it under the cursor, like macOS
                    let r=self.window_monitor(idx);
                    if let Some(w) = self.windows.iter_mut().find(|w| w.id == win_id) {
                        w.unmaximize_on((pos.x,pos.y),r);
                    }
                }
                let win = self.windows.iter_mut().find(|w| w.id == win_id).unwrap();
                win.resize_anchor = None;
                DragOperation::MoveWindow {
                    win_id,
                    start_cursor: (pos.x, pos.y),
                    start_pos: (win.x, win.y),
                }
            }
            Some(edges) => {
                let win = &mut self.windows[idx];
                win.set_resizing(true);
                win.resize_anchor = Some(ResizeAnchor {
                    edges,
                    right: win.x + win.width,
                    bottom: win.y + win.total_height(),
                });
                DragOperation::ResizeWindow {
                    win_id,
                    start_cursor: (pos.x, pos.y),
                    start_rect: (win.x, win.y, win.width, win.height),
                    edges,
                }
            }
        };
        self.active_drag = Some(op);

        // Take the pointer away from the client for the duration of the move/resize, like a
        // real compositor grab: end its implicit click grab and send wl_pointer.leave.
        // Chromium otherwise believes its button is still down and its drag regions
        // (Claude's title bar etc.) stop reacting to later clicks.
        let pointer = self.pointer.clone();
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.start_time.elapsed().as_millis() as u32;
        pointer.unset_grab(self, serial, time);
        pointer.motion(
            self,
            None,
            &smithay::input::pointer::MotionEvent {
                location: pos,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }
}

impl BufferHandler for FeatherCompositor {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl FractionalScaleHandler for FeatherCompositor {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let fullscreen = self.windows.iter().any(|w| w.fullscreen && w.wl_surface() == Some(&surface));
        let scale = crate::xwayland::preferred_scale(crate::xwayland::is_xwayland_surface(&surface), self.xwayland_hidpi, fullscreen, self.scale_factor);
        with_states(&surface, |states| {
            fractional_scale::with_fractional_scale(states, |fs| {
                fs.set_preferred_scale(scale);
            });
        });
    }
}

impl XdgShellHandler for FeatherCompositor {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        if let Some(w) = self.window_for_toplevel_mut(surface.wl_surface()) {
            if w.sync_app_metadata() {
                self.dock_model_dirty = true;
                self.shell.topbar_dirty = true;
                self.shell.switcher_dirty = true;
                self.needs_redraw = true;
                let top = self.windows.iter().rposition(|w| w.is_visible() && !self.auxiliary_game_window(w));
                if self.windows.iter().position(|w| w.activated) != top { self.refresh_focus(); }
            }
        }
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        self.title_changed(surface);
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // The window stays unmapped (invisible, no input, no focus) until the client attaches
        // a buffer. The initial configure is sent from the commit handler, as xdg-shell requires.
        let id = self.next_win_id;
        self.next_win_id += 1;
        tracing::debug!(id, "new toplevel");
        self.windows.push(ManagedWindow::new_wayland(id, surface));
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(idx) = self.window_index_for_toplevel(surface.wl_surface()) {
            let id = self.windows[idx].id;
            tracing::debug!(id, "toplevel destroyed");
            self.windows.remove(idx);
            if matches!(self.active_drag, Some(DragOperation::MoveWindow { win_id, .. }) | Some(DragOperation::ResizeWindow { win_id, .. }) if win_id == id)
            {
                self.active_drag = None;
            }
            self.shell.topbar_dirty = true;
            self.shell.dock_dirty = true;
            self.drop_keyboard_focus_if(surface.wl_surface());
            self.refresh_focus();
            self.needs_redraw = true;
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        let geometry = self.unconstrain_popup(&surface, &positioner);
        surface.with_pending_state(|state| {
            state.geometry = geometry;
            state.positioner = positioner;
        });
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!("Не удалось отследить popup: {err:?}");
        }
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        let geometry = self.unconstrain_popup(&surface, &positioner);
        surface.with_pending_state(|state| {
            state.geometry = geometry;
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
        let _ = surface.send_configure();
        self.needs_redraw = true;
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // Popups are dismissed by clicks outside of them (see input handling)
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {
        self.needs_redraw = true;
    }

    fn move_request(&mut self, surface: ToplevelSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        self.begin_client_drag(&surface, None);
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        self.begin_client_drag(&surface, Some(Edges::from_xdg(edges)));
    }

    fn maximize_request(&mut self,surface:ToplevelSurface){
        if let Some(i)=self.window_index_for_toplevel(surface.wl_surface()){let r=self.window_monitor(i);self.windows[i].resize_anchor=None;self.windows[i].maximize_on(true,r);}
        if surface.is_initial_configure_sent(){surface.send_pending_configure();}self.needs_redraw=true;
    }
    fn unmaximize_request(&mut self,surface:ToplevelSurface){
        if let Some(i)=self.window_index_for_toplevel(surface.wl_surface()){let r=self.window_monitor(i);self.windows[i].maximize_on(false,r);}
        if surface.is_initial_configure_sent(){surface.send_pending_configure();}self.needs_redraw=true;
    }
    fn fullscreen_request(&mut self,surface:ToplevelSurface,output:Option<wl_output::WlOutput>){
        if let Some(i)=self.window_index_for_toplevel(surface.wl_surface()){
            let r=output.as_ref().and_then(|o|self.monitors.iter().find(|m|m.output.owns(o))).map(|m|m.info.rect).unwrap_or_else(||self.window_monitor(i));
            self.windows[i].resize_anchor=None;self.set_window_fullscreen(i,true,r);
        }
        if surface.is_initial_configure_sent(){surface.send_pending_configure();}self.needs_redraw=true;
    }
    fn unfullscreen_request(&mut self,surface:ToplevelSurface){
        if let Some(i)=self.window_index_for_toplevel(surface.wl_surface()){let r=self.window_monitor(i);self.set_window_fullscreen(i,false,r);}
        if surface.is_initial_configure_sent(){surface.send_pending_configure();}self.needs_redraw=true;
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        if let Some(i) = self.window_index_for_toplevel(surface.wl_surface()) {
            self.minimize_window(i);
        }
    }
}

impl FeatherCompositor {
    pub fn minimize_window(&mut self, idx: usize) {
        self.windows[idx].minimized = true;
        self.shell.dock_dirty = true;
        self.shell.topbar_dirty = true;
        self.refresh_focus();
        self.needs_redraw = true;
    }
}

impl XdgDecorationHandler for FeatherCompositor {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // We prefer server-side decorations, but the client has the final word (request_mode)
        self.set_decoration_mode(&toplevel, DecorationMode::ServerSide);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        // Electron/Chromium frameless windows (Claude Desktop, VS Code…) and Firefox ask for
        // client-side decorations: forcing ours on top produced a "window inside a window".
        self.set_decoration_mode(&toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.set_decoration_mode(&toplevel, DecorationMode::ServerSide);
    }
}

impl FeatherCompositor {
    fn set_decoration_mode(&mut self, toplevel: &ToplevelSurface, mode: DecorationMode) {
        let ssd = mode == DecorationMode::ServerSide;
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(mode);
        });
        if let Some(win) = self.window_for_toplevel_mut(toplevel.wl_surface()) {
            if win.ssd != ssd {
                win.ssd = ssd;
                win.dirty = true;
            }
        }
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
        self.needs_redraw = true;
    }
}

impl SelectionHandler for FeatherCompositor {
    /// Clipboard contents the compositor itself offers (screenshots)
    type SelectionUserData = crate::screenshot::ClipboardData;

    fn send_selection(
        &mut self,
        _ty: smithay::wayland::selection::SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        user_data: &Self::SelectionUserData,
    ) {
        crate::screenshot::serve(&mime_type, fd, user_data);
    }

    fn new_selection(&mut self, ty: smithay::wayland::selection::SelectionTarget, source: Option<smithay::wayland::selection::SelectionSource>, _seat: Seat<Self>) {
        if ty == smithay::wayland::selection::SelectionTarget::Clipboard {
            self.client_clipboard_mimes = source.map(|s| s.mime_types()).unwrap_or_default();
        }
    }
}

impl DataDeviceHandler for FeatherCompositor {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for FeatherCompositor {
    fn started(&mut self, source: Option<smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource>, icon: Option<WlSurface>, _seat: Seat<Self>) {
        self.client_drag_started(source, icon);
    }
    fn dropped(&mut self, target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        self.client_drag_dropped(target);
    }
}

/// Desktop icons dragged into programs
impl ServerDndGrabHandler for FeatherCompositor {
    fn send(&mut self, mime_type: String, fd: OwnedFd, _seat: Seat<Self>) {
        self.desktop_dnd_send(&mime_type, fd);
    }
    fn dropped(&mut self, _seat: Seat<Self>) {
        self.desktop_dnd_dropped();
    }
    fn cancelled(&mut self, _seat: Seat<Self>) {
        self.desktop_dnd_cancelled();
    }
    fn finished(&mut self, _seat: Seat<Self>) {
        self.desktop_dnd_finished();
    }
}

impl PrimarySelectionHandler for FeatherCompositor {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl CompositorHandler for FeatherCompositor {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        add_pre_commit_hook::<Self, _>(surface, |state, _dh, surface| {
            let acquire_point = with_states(surface, |states| {
                states
                    .cached_state
                    .get::<DrmSyncobjCachedState>()
                    .pending()
                    .acquire_point
                    .clone()
            });
            if let Some(acquire_point) = acquire_point {
                if let Ok((blocker, source)) = acquire_point.generate_blocker() {
                    if let Some(client) = surface.client() {
                        add_blocker(surface, blocker);
                        let _ = state.loop_handle.insert_source(source, move |(), (), state| {
                            let dh = state.display_handle.clone();
                            state.client_compositor_state(&client).blocker_cleared(state, &dh);
                            Ok(())
                        });
                    }
                }
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.lock_surface_commit(surface);
        self.needs_redraw = true;
        if let Some(stats) = self.frame_stats.as_mut() {
            stats.record_commit();
        }

        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        if &root == surface {
            self.handle_toplevel_commit(surface);
        }

        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(ref popup)) = self.popups.find_popup(surface) {
            if !popup.is_initial_configure_sent() {
                let _ = popup.send_configure();
            }
        }
    }
}

impl DrmSyncobjHandler for FeatherCompositor {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.drm_syncobj_state.as_mut()
    }
}

impl ShmHandler for FeatherCompositor {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl DmabufHandler for FeatherCompositor {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        // Really import the buffer so broken GPU buffers fail early instead of rendering black
        if self.backend.renderer().import_dmabuf(&dmabuf, None).is_ok() {
            let _ = notifier.successful::<Self>();
        } else {
            notifier.failed();
        }
    }
}

impl OutputHandler for FeatherCompositor {}

impl SeatHandler for FeatherCompositor {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        // Clipboard / primary selection follow keyboard focus
        let dh = &self.display_handle;
        let client = if self.lock.active {None}else{focused.and_then(|s| dh.get_client(s.id()).ok())};
        smithay::wayland::selection::data_device::set_data_device_focus(dh, seat, client.clone());
        smithay::wayland::selection::primary_selection::set_primary_focus(dh, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.apply_cursor(image);
    }
}

impl TabletSeatHandler for FeatherCompositor {}

impl XdgActivationHandler for FeatherCompositor {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Honour fresh tokens only, so background apps can't steal focus forever
        if token_data.timestamp.elapsed().as_secs() < 10 {
            if let Some(idx) = self.window_index_for_toplevel(&surface) {
                let mut win = self.windows.remove(idx);
                win.minimized = false;
                self.windows.push(win);
                self.refresh_focus();
                self.shell.dock_dirty = true;
                self.needs_redraw = true;
            }
        }
        self.xdg_activation_state.remove_token(&token);
    }
}

delegate_xdg_shell!(FeatherCompositor);
delegate_xdg_decoration!(FeatherCompositor);
delegate_xdg_activation!(FeatherCompositor);
delegate_compositor!(FeatherCompositor);
delegate_shm!(FeatherCompositor);
delegate_dmabuf!(FeatherCompositor);
delegate_drm_syncobj!(FeatherCompositor);
delegate_seat!(FeatherCompositor);
delegate_data_device!(FeatherCompositor);
delegate_primary_selection!(FeatherCompositor);
delegate_output!(FeatherCompositor);
delegate_viewporter!(FeatherCompositor);
delegate_fractional_scale!(FeatherCompositor);
delegate_cursor_shape!(FeatherCompositor);
