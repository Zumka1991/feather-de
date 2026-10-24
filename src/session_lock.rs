//! A locked scene contains only the lock client (or the built-in PAM panel).
//! Losing the lock client never restores the desktop. Confirmation follows presentation.
use crate::{
    render::{FeatherElement, SceneElement},
    text::FontSystem,
    theme::*,
    FeatherCompositor,
};
use feather_ui::input::TextInput;
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind,
            },
            gles::GlesRenderer,
        },
    },
    reexports::{
        calloop::channel,
        wayland_protocols::ext::session_lock::v1::server::{
            ext_session_lock_manager_v1::ExtSessionLockManagerV1,
            ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
            ext_session_lock_v1::{self, ExtSessionLockV1},
        },
        wayland_server::{
            protocol::{wl_output::WlOutput, wl_surface::WlSurface},
            Client, DataInit, Dispatch, DisplayHandle, Resource,
        },
    },
    utils::{Rectangle, Transform},
    wayland::session_lock::{
        ExtLockSurfaceUserData, LockSurface, SessionLockHandler, SessionLockManagerGlobalData,
        SessionLockManagerState, SessionLockState, SessionLocker,
    },
};

pub struct Lock {
    pub active: bool,
    pub owner: Option<ExtSessionLockV1>,
    pub surface: Option<LockSurface>,
    pending: Option<SessionLocker>,
    confirmed: bool,
    generation: u64,
    input: TextInput,
    verifying: bool,
    message: &'static str,
    caps: bool,
    layout: String,
    dirty: bool,
    buffer: MemoryRenderBuffer,
    scale: f64,
    phys: (i32, i32),
    pub protected_frame: bool,
    pub display_protected: bool,
    was_external: bool,
    retired_owner: Option<ExtSessionLockV1>,
}
impl Default for Lock {
    fn default() -> Self {
        Self {
            active: false,
            owner: None,
            surface: None,
            pending: None,
            confirmed: false,
            generation: 0,
            input: TextInput::default().with_byte_limit(1024),
            verifying: false,
            message: "Введите пароль вашей учётной записи",
            caps: false,
            layout: String::new(),
            dirty: true,
            buffer: MemoryRenderBuffer::new(Fourcc::Abgr8888, (1, 1), 1, Transform::Normal, None),
            scale: 0.0,
            phys: (1, 1),
            protected_frame: false,
            display_protected: false,
            was_external: false,
            retired_owner: None,
        }
    }
}
impl Lock {
    pub fn external(&self) -> bool {
        self.owner.as_ref().is_some_and(Resource::is_alive)
    }
    pub fn focus(&self) -> Option<WlSurface> {
        if !self.external() {
            return None;
        }
        self.surface
            .as_ref()
            .filter(|s| s.alive())
            .map(|s| s.wl_surface().clone())
            .filter(|s| {
                smithay::backend::renderer::utils::with_renderer_surface_state(s, |r| {
                    r.buffer().is_some()
                })
                .unwrap_or(false)
            })
    }
    pub fn key(&mut self, sym: u32, ch: Option<char>, ctrl: bool, shift: bool, caps: bool) -> bool {
        if self.verifying {
            return false;
        }
        self.caps = caps;
        self.dirty = true;
        if sym == 0xff0d || sym == 0xff8d {
            return true;
        }
        if sym == 0xff1b {
            self.input.clear();
            return false;
        }
        let mut bytes = [0u8; 4];
        let utf8 = ch.map(|c| c.encode_utf8(&mut bytes) as &str).unwrap_or("");
        self.input.key(sym, Some(utf8), ctrl, shift);
        zeroize::Zeroize::zeroize(&mut bytes);
        false
    }
    pub fn click_field(&mut self, fonts: &mut FontSystem, x: f32, extend: bool) {
        if !self.verifying {
            self.input
                .click_masked(fonts, x - 46.0, 308.0, 14.0, extend);
            self.dirty = true;
        }
    }
    pub fn paint(&mut self, fonts: &mut FontSystem, scale: f64, layout: &str) {
        if !self.active || self.external() {
            return;
        }
        if self.layout != layout {
            self.layout.clear();
            self.layout.push_str(layout);
            self.dirty = true;
        }
        if !self.dirty && self.scale == scale {
            return;
        }
        self.scale = scale;
        self.dirty = false;
        let size = (
            (400.0 * scale).round() as i32,
            (260.0 * scale).round() as i32,
        );
        if self.phys != size {
            self.phys = size;
            self.buffer =
                MemoryRenderBuffer::new(Fourcc::Abgr8888, size, 1, Transform::Normal, None);
        }
        let mut ctx = self.buffer.render();
        let _ = ctx.draw(|buf| {
            let mut pm =
                tiny_skia::PixmapMut::from_bytes(buf, size.0 as u32, size.1 as u32).unwrap();
            pm.fill(rgba(15, 18, 31, 255));
            let white = rgba(245, 247, 255, 255);
            let dim = rgba(174, 184, 207, 255);
            let accent = rgba(108, 162, 255, 255);
            draw_feather_logo(&mut pm, 200.0, 35.0, 2.5, white);
            fonts.draw_text_centered(
                &mut pm,
                "Экран заблокирован",
                200.0,
                76.0,
                21.0,
                white,
                true,
            );
            fill_rounded_rect(
                &mut pm,
                34.0,
                119.0,
                332.0,
                38.0,
                10.0,
                rgba(32, 39, 58, 255),
            );
            fonts.draw_text_centered(&mut pm, &self.layout, 345.0, 104.0, 10.0, dim, true);
            self.input
                .draw_masked(&mut pm, fonts, 46.0, 130.0, 308.0, 14.0, white, accent);
            let msg = if self.verifying {
                "Проверка…"
            } else if self.caps {
                "Включён Caps Lock"
            } else {
                self.message
            };
            fonts.draw_text_clipped_centered(&mut pm, msg, 200.0, 173.0, 370.0, 11.0, dim, false);
            fill_rounded_rect(&mut pm, 115.0, 204.0, 170.0, 32.0, 9.0, accent);
            fonts.draw_text_centered(
                &mut pm,
                "Разблокировать ↵",
                200.0,
                214.0,
                12.0,
                rgba(10, 20, 40, 255),
                true,
            );
            Result::<_, ()>::Ok(vec![Rectangle::from_size(size.into())])
        });
    }
    pub fn elements(
        &self,
        renderer: &mut GlesRenderer,
        scale: f64,
        screen: (i32, i32),
    ) -> Vec<FeatherElement> {
        if self.external() {
            let mut out = Vec::new();
            if let Some(surface) = self.focus() {
                crate::render::push_surface_tree(
                    &mut out,
                    renderer,
                    &surface,
                    (0, 0).into(),
                    scale,
                    Default::default(),
                );
            }
            return out;
        }
        let loc = (
            ((screen.0 as f64 - 400.0) * scale / 2.0).round(),
            ((screen.1 as f64 - 260.0) * scale / 2.0).round(),
        );
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            loc,
            &self.buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .map(|e| vec![SceneElement::Memory(e).into()])
        .unwrap_or_default()
    }
}

impl FeatherCompositor {
    pub fn lock_session(&mut self) {
        if self.lock.active {
            return;
        }
        self.lock.active = true;
        self.lock.display_protected = false;
        self.lock.protected_frame = false;
        self.lock.generation += 1;
        self.lock.dirty = true;
        self.power.locked_hint(true);
        self.lock.input.clear();
        self.lock.message = "Введите пароль вашей учётной записи";
        self.cancel_lock_input();
        self.shell.close_menus();
        self.shell.switcher = None;
        self.icon_drag = None;
        self.screenshot_select = None;
        self.apply_cursor(smithay::input::pointer::CursorImageStatus::default_named());
        self.refresh_focus();
        self.refresh_pointer_focus();
        tracing::info!("Сессия заблокирована");
    }
    fn cancel_lock_input(&mut self) {
        self.shell.desktop.cancel_interaction();
        let keyboard = self.keyboard.clone();
        keyboard.unset_grab(self);
        self.refresh_focus();
        let pointer = self.pointer.clone();
        // Clear a DnD grab's target before unsetting it; Smithay's unset calls drop().
        pointer.motion(
            self,
            None,
            &smithay::input::pointer::MotionEvent {
                location: self.pointer_location,
                serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                time: self.start_time.elapsed().as_millis() as u32,
            },
        );
        pointer.unset_grab(
            self,
            smithay::utils::SERIAL_COUNTER.next_serial(),
            self.start_time.elapsed().as_millis() as u32,
        );
        pointer.motion(
            self,
            None,
            &smithay::input::pointer::MotionEvent {
                location: self.pointer_location,
                serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                time: self.start_time.elapsed().as_millis() as u32,
            },
        );
        pointer.frame(self);
        // Cancel DnD before synthesizing releases: releasing its last button would drop files.
        self.release_all_input();
    }
    fn unlock_session(&mut self) {
        if self.power.sleeping {
            return;
        }
        self.cancel_lock_input();
        self.lock.active = false;
        self.lock.generation += 1;
        self.lock.retired_owner = self.lock.owner.take();
        self.lock.was_external = false;
        self.lock.surface = None;
        self.lock.pending = None;
        self.lock.confirmed = false;
        self.lock.input.clear();
        self.lock.verifying = false;
        self.lock.buffer =
            MemoryRenderBuffer::new(Fourcc::Abgr8888, (1, 1), 1, Transform::Normal, None);
        self.lock.phys = (1, 1);
        self.lock.scale = 0.0;
        self.lock.protected_frame = false;
        self.lock.display_protected = false;
        self.power.locked_hint(false);
        self.activity();
        self.refresh_focus();
        self.refresh_pointer_focus();
        tracing::info!("Сессия разблокирована");
    }
    pub fn lock_submit(&mut self) {
        if !self.lock.active || self.lock.external() || self.lock.verifying || self.power.sleeping {
            return;
        }
        self.lock.verifying = true;
        self.lock.dirty = true;
        self.needs_redraw = true;
        let generation = self.lock.generation;
        let password = self.lock.input.take_text();
        let (tx, rx) = channel::channel();
        if self
            .loop_handle
            .insert_source(rx, move |event, _, st| {
                if let channel::Event::Msg(result) = event {
                    if st.lock.active && st.lock.generation == generation && !st.lock.external() {
                        st.lock.verifying = false;
                        st.lock.dirty = true;
                        st.needs_redraw = true;
                        match result {
                            Ok(true) => st.unlock_session(),
                            Ok(false) => st.lock.message = "Неверный пароль или вход недоступен",
                            Err(message) => st.lock.message = message,
                        }
                    }
                }
            })
            .is_err()
        {
            self.lock.verifying = false;
            self.lock.message = "Не удалось начать проверку";
            return;
        }
        if std::thread::Builder::new()
            .name("feather-unlock".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                let _ = tx.send(crate::lock_auth::authenticate(password));
            })
            .is_err()
        {
            self.lock.verifying = false;
            self.lock.message = "Не удалось начать проверку";
        }
    }
    /// Called only after the protected frame reached the display (DRM vblank/host frame).
    pub fn lock_presented(&mut self) {
        if !self.lock.protected_frame {
            return;
        }
        self.lock.protected_frame = false;
        self.lock.display_protected = true;
        if self.power.sleeping {
            self.power.protected();
        }
        if let Some(confirmation) = self.lock.pending.take() {
            self.lock.confirmed = true;
            confirmation.lock();
        }
    }
    pub fn lock_sleep_barrier(&mut self) {
        self.lock.generation += 1;
        self.lock.verifying = false;
        self.lock.input.clear();
        self.lock.dirty = true;
        self.lock_session();
        if self.power.blanked || self.lock.display_protected {
            self.power.protected();
        }
        self.needs_redraw = true;
    }
    pub fn refresh_lock(&mut self) {
        if self.lock.active && self.lock.was_external && !self.lock.external() {
            self.lock.was_external = false;
            self.lock.dirty = true;
            self.lock.surface = None;
            self.refresh_focus();
            self.refresh_pointer_focus();
            tracing::warn!("Приложение блокировки завершилось; сессия остаётся заблокированной");
        }
    }
    pub fn lock_surface_commit(&mut self, surface: &WlSurface) {
        if self.lock.active && self.lock.focus().as_ref() == Some(surface) {
            self.refresh_focus();
        }
    }
    pub fn lock_resized(&mut self) {
        if let Some(surface) = &self.lock.surface {
            surface.with_pending_state(|s| {
                s.size = Some((self.shell.screen_w as u32, self.shell.screen_h as u32).into())
            });
            surface.send_configure();
        }
    }
}
impl SessionLockHandler for FeatherCompositor {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }
    fn lock(&mut self, confirmation: SessionLocker) {
        if self.lock.active {
            return;
        }
        self.lock_session();
        self.lock.owner = Some(confirmation.ext_session_lock().clone());
        self.lock.was_external = true;
        self.lock.pending = Some(confirmation);
    }
    fn unlock(&mut self) {
        self.unlock_session();
        self.lock.retired_owner = None;
    }
    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        if !self.output.owns(&output) {
            return;
        }
        self.lock.surface = Some(surface);
        self.lock_resized();
    }
}
// Smithay 0.7 calls unlock() even after posting InvalidUnlock. Gate requests before
// delegation so a rejected/second lock object cannot unlock the accepted owner's lock.
impl Dispatch<ExtSessionLockV1, SessionLockState> for FeatherCompositor {
    fn request(
        st: &mut Self,
        client: &Client,
        resource: &ExtSessionLockV1,
        request: ext_session_lock_v1::Request,
        data: &SessionLockState,
        dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if !matches!(request, ext_session_lock_v1::Request::Destroy)
            && (st.lock.owner.as_ref() != Some(resource)
                || (matches!(request, ext_session_lock_v1::Request::UnlockAndDestroy)
                    && !st.lock.confirmed))
        {
            resource.post_error(
                ext_session_lock_v1::Error::InvalidUnlock,
                "Not the confirmed lock owner",
            );
            return;
        }
        <SessionLockManagerState as Dispatch<ExtSessionLockV1, SessionLockState, Self>>::request(
            st, client, resource, request, data, dh, init,
        );
    }
}
smithay::reexports::wayland_server::delegate_global_dispatch!(FeatherCompositor:[ExtSessionLockManagerV1:SessionLockManagerGlobalData]=>SessionLockManagerState);
// Smithay keeps output bookkeeping after a lock client disconnects. Once PAM has
// unlocked that orphaned lock, retire its protocol state before accepting another lock.
impl Dispatch<ExtSessionLockManagerV1, ()> for FeatherCompositor {
    fn request(
        st: &mut Self,
        client: &Client,
        resource: &ExtSessionLockManagerV1,
        request:smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::Request,
        data: &(),
        dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::Request;
        if matches!(request, Request::Lock { .. }) && !st.lock.active {
            if let Some(owner) = st.lock.retired_owner.take() {
                if let Some(old) = owner.data::<SessionLockState>() {
                    <SessionLockManagerState as Dispatch<
                        ExtSessionLockV1,
                        SessionLockState,
                        Self,
                    >>::request(
                        st,
                        client,
                        &owner,
                        ext_session_lock_v1::Request::UnlockAndDestroy,
                        old,
                        dh,
                        init,
                    );
                }
            }
        }
        <SessionLockManagerState as Dispatch<ExtSessionLockManagerV1, (), Self>>::request(
            st, client, resource, request, data, dh, init,
        );
    }
}
smithay::reexports::wayland_server::delegate_dispatch!(FeatherCompositor:[ExtSessionLockSurfaceV1:ExtLockSurfaceUserData]=>SessionLockManagerState);
