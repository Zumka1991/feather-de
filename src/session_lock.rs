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
    pub extra_surfaces: Vec<(String,LockSurface)>,
    pending: Option<SessionLocker>,
    confirmed: bool,
    generation: u64,
    input: TextInput,
    verifying: bool,
    message: &'static str,
    caps: bool,
    layout: String,
    dirty: bool,
    /// Account panel at the bottom: avatar, name, password field
    buffer: MemoryRenderBuffer,
    /// Date and large time at the top; repainted when the minute changes
    clock_buffer: MemoryRenderBuffer,
    clock: String,
    /// 4×4 black, stretched over the wallpaper so white text reads on light ones
    dim: MemoryRenderBuffer,
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
            extra_surfaces: Vec::new(),
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
            clock_buffer: MemoryRenderBuffer::new(Fourcc::Abgr8888, (1, 1), 1, Transform::Normal, None),
            clock: String::new(),
            dim: {
                let mut dim = MemoryRenderBuffer::new(Fourcc::Abgr8888, (4, 4), 1, Transform::Normal, None);
                let _ = dim.render().draw(|buf| {
                    buf.chunks_exact_mut(4).for_each(|px| px.copy_from_slice(&[0, 0, 0, 255]));
                    Result::<_, ()>::Ok(vec![Rectangle::from_size((4, 4).into())])
                });
                dim
            },
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
    /// Account panel origin (logical): bottom centre, like the macOS login window
    fn panel_origin(screen: (i32, i32)) -> (f32, f32) {
        ((screen.0 as f32 - PANEL_W) * 0.5, (screen.1 as f32 - PANEL_H - (screen.1 as f32 * 0.08).max(40.0)).max(CLOCK_H + 20.0))
    }
    fn clock_origin(screen: (i32, i32)) -> (f32, f32) {
        ((screen.0 as f32 - CLOCK_W) * 0.5, (screen.1 as f32 * 0.07).clamp(24.0, 90.0))
    }

    /// A click on the built-in panel; true when it asks to submit the password
    pub fn click(&mut self, fonts: &mut FontSystem, x: f64, y: f64, screen: (i32, i32), extend: bool) -> bool {
        if self.verifying || self.external() {
            return false;
        }
        let (ox, oy) = Self::panel_origin(screen);
        let (x, y) = (x as f32 - ox, y as f32 - oy);
        let (fx, fy, fw, fh) = FIELD;
        if !(fy..fy + fh).contains(&y) || !(fx..fx + fw).contains(&x) {
            return false;
        }
        if x >= fx + fw - fh {
            return !self.input.is_empty();
        }
        self.input.click_masked(fonts, x - (fx + 16.0), fw - 16.0 - fh, 14.0, extend);
        self.dirty = true;
        false
    }

    /// Wallpaper or glass changed under the built-in panel
    pub fn repaint(&mut self) {
        if self.active {
            self.dirty = true;
            self.clock.clear();
        }
    }

    /// The clock shows a different minute: the next frame repaints it
    pub fn clock_stale(&self) -> bool {
        self.active && !self.external() && self.clock != clock_text().1
    }

    pub fn paint(&mut self, fonts: &mut FontSystem, scale: f64, layout: &str, screen: (i32, i32)) {
        if !self.active || self.external() {
            return;
        }
        if self.layout != layout {
            self.layout.clear();
            self.layout.push_str(layout);
            self.dirty = true;
        }
        let rescaled = self.scale != scale;
        let (date, time) = clock_text();
        if rescaled || self.clock != time {
            self.paint_clock(fonts, scale, screen, &date, &time);
            self.clock = time;
        }
        if !self.dirty && !rescaled {
            return;
        }
        self.scale = scale;
        self.dirty = false;
        let size = ((PANEL_W as f64 * scale).round() as i32, (PANEL_H as f64 * scale).round() as i32);
        if self.phys != size {
            self.phys = size;
            self.buffer = MemoryRenderBuffer::new(Fourcc::Abgr8888, size, 1, Transform::Normal, None);
        }
        let (ox, oy) = Self::panel_origin(screen);
        set_glass_origin(ox, oy);
        let name = account_name();
        let mut ctx = self.buffer.render();
        let _ = ctx.draw(|buf| {
            buf.fill(0);
            let mut pm = tiny_skia::PixmapMut::from_bytes(buf, size.0 as u32, size.1 as u32).unwrap();
            let white = rgba(255, 255, 255, 255);
            let soft = rgba(255, 255, 255, 205);
            let accent = rgba(255, 255, 255, 255);
            let cx = PANEL_W * 0.5;
            // Avatar: frosted disc with the initial
            let r = 34.0;
            fill_frosted(&mut pm, cx - r, 6.0, r * 2.0, r * 2.0, r);
            fill_rounded_rect(&mut pm, cx - r, 6.0, r * 2.0, r * 2.0, r, rgba(255, 255, 255, 70));
            stroke_rounded_rect(&mut pm, cx - r + 0.5, 6.5, r * 2.0 - 1.0, r * 2.0 - 1.0, r - 0.5, 1.0, rgba(255, 255, 255, 110));
            let initial: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
            shadowed(&mut pm, fonts, &initial, cx, 6.0 + r - 17.0, 30.0, white, true);
            shadowed(&mut pm, fonts, &name, cx, 86.0, 16.0, white, true);
            // Password field: frosted pill with the arrow button inside
            let (fx, fy, fw, fh) = FIELD;
            fill_frosted(&mut pm, fx, fy, fw, fh, fh * 0.5);
            fill_rounded_rect(&mut pm, fx, fy, fw, fh, fh * 0.5, rgba(255, 255, 255, 64));
            stroke_rounded_rect(&mut pm, fx + 0.5, fy + 0.5, fw - 1.0, fh - 1.0, fh * 0.5 - 0.5, 1.0, rgba(255, 255, 255, 90));
            let text_w = fw - 16.0 - fh;
            if self.input.is_empty() && !self.verifying {
                fonts.draw_text_clipped(&mut pm, "Введите пароль", fx + 19.0, fy + 8.5, text_w, 13.0, rgba(255, 255, 255, 150), false);
            }
            self.input.draw_masked(&mut pm, fonts, fx + 16.0, fy + 9.0, text_w, 14.0, white, accent);
            let (bx, by, bd) = (fx + fw - fh + 4.0, fy + 4.0, fh - 8.0);
            let ready = !self.input.is_empty() && !self.verifying;
            fill_rounded_rect(&mut pm, bx, by, bd, bd, bd * 0.5, rgba(255, 255, 255, if ready { 200 } else { 70 }));
            fonts.draw_text_centered(&mut pm, "→", bx + bd * 0.5, by + 3.5, 14.0, if ready { rgba(30, 30, 40, 255) } else { rgba(255, 255, 255, 170) }, true);
            // Input source, like the menu at the top right of the macOS login window
            if !self.layout.is_empty() {
                let lx = fx + fw + 10.0;
                stroke_rounded_rect(&mut pm, lx, fy + 8.0, 30.0, 18.0, 5.0, 1.0, soft);
                fonts.draw_text_centered(&mut pm, &self.layout, lx + 15.0, fy + 10.5, 10.0, soft, true);
            }
            let msg = if self.verifying {
                "Проверка…"
            } else if self.caps {
                "Включён Caps Lock"
            } else {
                self.message
            };
            let w = PANEL_W - 20.0;
            let fitted = fonts.ellipsize(msg, w, 12.0, false);
            shadowed(&mut pm, fonts, &fitted, cx, fy + fh + 14.0, 12.0, soft, false);
            Result::<_, ()>::Ok(vec![Rectangle::from_size(size.into())])
        });
    }

    fn paint_clock(&mut self, fonts: &mut FontSystem, scale: f64, screen: (i32, i32), date: &str, time: &str) {
        let size = ((CLOCK_W as f64 * scale).round() as i32, (CLOCK_H as f64 * scale).round() as i32);
        self.clock_buffer = MemoryRenderBuffer::new(Fourcc::Abgr8888, size, 1, Transform::Normal, None);
        let (ox, oy) = Self::clock_origin(screen);
        set_glass_origin(ox, oy);
        let mut ctx = self.clock_buffer.render();
        let _ = ctx.draw(|buf| {
            let mut pm = tiny_skia::PixmapMut::from_bytes(buf, size.0 as u32, size.1 as u32).unwrap();
            let cx = CLOCK_W * 0.5;
            shadowed(&mut pm, fonts, date, cx, 6.0, 22.0, rgba(255, 255, 255, 235), true);
            shadowed(&mut pm, fonts, time, cx, 34.0, 96.0, rgba(255, 255, 255, 245), true);
            Result::<_, ()>::Ok(vec![Rectangle::from_size(size.into())])
        });
    }

    pub fn monitor_elements(&self,renderer:&mut GlesRenderer,scale:f64,info:&crate::monitors::Info,wallpaper:Option<(&smithay::backend::renderer::element::texture::TextureBuffer<smithay::backend::renderer::gles::GlesTexture>,(i32,i32))>)->Vec<FeatherElement>{
        if self.external(){
            let mut out=Vec::new();
            if let Some((_,s))=self.extra_surfaces.iter().find(|(name,_)|*name==info.name){
                if s.alive(){crate::render::push_surface_tree(&mut out,renderer,s.wl_surface(),(0,0).into(),scale,Default::default());}
            }
            return out;
        }
        if info.primary {self.elements(renderer,scale,(info.rect.w,info.rect.h),wallpaper)}else{Vec::new()}
    }
    pub fn elements(
        &self,
        renderer: &mut GlesRenderer,
        scale: f64,
        screen: (i32, i32),
        wallpaper: Option<(&smithay::backend::renderer::element::texture::TextureBuffer<smithay::backend::renderer::gles::GlesTexture>, (i32, i32))>,
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
        let at = |(x, y): (f32, f32)| ((x as f64 * scale).round(), (y as f64 * scale).round());
        let mut out: Vec<FeatherElement> = [(&self.buffer, Self::panel_origin(screen)), (&self.clock_buffer, Self::clock_origin(screen))]
            .into_iter()
            .filter_map(|(buffer, origin)| {
                MemoryRenderBufferRenderElement::from_buffer(renderer, at(origin), buffer, None, None, None, Kind::Unspecified).ok()
            })
            .map(|e| SceneElement::Memory(e).into())
            .collect();
        // The desktop wallpaper only: no window content is ever drawn while locked
        if let Some((texture, (w, h))) = wallpaper {
            let full = (crate::theme::to_phys(screen.0), crate::theme::to_phys(screen.1));
            if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
                renderer, (0.0, 0.0), &self.dim, Some(0.2), Some(Rectangle::from_size((4.0, 4.0).into())), Some(full.into()), Kind::Unspecified,
            ) {
                out.push(SceneElement::Memory(e).into());
            }
            out.push(SceneElement::Texture(smithay::backend::renderer::element::texture::TextureRenderElement::from_texture_buffer(
                (0.0, 0.0), texture, None, Some(Rectangle::from_size((w as f64, h as f64).into())), Some(full.into()), Kind::Unspecified,
            )).into());
        }
        out
    }
}

const PANEL_W: f32 = 380.0;
const PANEL_H: f32 = 200.0;
const CLOCK_W: f32 = 640.0;
const CLOCK_H: f32 = 150.0;
/// Password field in panel coordinates: x, y, width, height
const FIELD: (f32, f32, f32, f32) = (70.0, 118.0, 240.0, 34.0);

/// Centred white text with a soft shadow, readable on any wallpaper
#[allow(clippy::too_many_arguments)]
fn shadowed(pm: &mut tiny_skia::PixmapMut, fonts: &mut FontSystem, text: &str, cx: f32, y: f32, size: f32, color: tiny_skia::Color, bold: bool) {
    // A wide, faint shadow: a few offsets approximate a blur without a blur pass
    let spread = (size * 0.02).max(0.6);
    for (dx, dy) in [(-spread, spread), (spread, spread), (0.0, spread * 2.0)] {
        fonts.draw_text_centered(pm, text, cx + dx, y + dy, size, rgba(0, 0, 0, 22), bold);
    }
    fonts.draw_text_centered(pm, text, cx, y, size, color, bold);
}

/// («суббота, 24 октября», «8:36»)
fn clock_text() -> (String, String) {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    feather_ui::i18n::format_lock_clock(
        now.weekday().num_days_from_monday() as usize,
        now.day(),
        now.month0() as usize,
        now.hour(),
        now.minute(),
    )
}

/// Full name from the account (GECOS), else the login; read once
fn account_name() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result = std::ptr::null_mut();
        let mut buf = vec![0u8; 16384];
        if libc::getpwuid_r(libc::getuid(), &mut pwd, buf.as_mut_ptr().cast(), buf.len(), &mut result) != 0 || result.is_null() {
            return String::new();
        }
        let text = |p: *const libc::c_char| if p.is_null() { String::new() } else { std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned() };
        let gecos = text(pwd.pw_gecos);
        let full = gecos.split(',').next().unwrap_or("").trim().to_string();
        if full.is_empty() { text(pwd.pw_name) } else { full }
    }).clone()
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
        self.cancel_scene_input();
        self.shell.close_menus();
        self.shell.switcher = None;
        self.icon_drag = None;
        self.screenshot_select = None;
        self.apply_cursor(smithay::input::pointer::CursorImageStatus::default_named());
        self.refresh_focus();
        self.refresh_pointer_focus();
        tracing::info!("Сессия заблокирована");
    }
    pub(crate) fn cancel_scene_input(&mut self) {
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
        self.cancel_scene_input();
        self.lock.active = false;
        self.lock.generation += 1;
        self.lock.retired_owner = self.lock.owner.take();
        self.lock.was_external = false;
        self.lock.surface = None;
        self.lock.extra_surfaces.clear();
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
        self.lock.extra_surfaces.clear();
            self.refresh_focus();
            self.refresh_pointer_focus();
            tracing::warn!("Приложение блокировки завершилось; сессия остаётся заблокированной");
        }
    }
    pub fn lock_surface_commit(&mut self, surface: &WlSurface) {
        if self.lock.active && self.lock.extra_surfaces.iter().any(|(_,s)|s.wl_surface()==surface) {
            self.refresh_focus();
        }
    }
    pub fn lock_focus(&self)->Option<WlSurface>{
        if !self.lock.external(){return None;}
        let r=self.monitor_at(self.pointer_location.x,self.pointer_location.y);
        let m=self.monitors.iter().find(|m|m.info.rect==r)?;
        let (_,s)=self.lock.extra_surfaces.iter().find(|(name,_)|*name==m.info.name)?;
        s.alive().then(||s.wl_surface().clone()).filter(|s|smithay::backend::renderer::utils::with_renderer_surface_state(s,|r|r.buffer().is_some()).unwrap_or(false))
    }
    pub fn lock_resized(&mut self) {
        self.lock.surface=None;
        for (name,surface) in &self.lock.extra_surfaces {
            if let Some(m)=self.monitors.iter().find(|m|m.info.name==*name){
                surface.with_pending_state(|s|s.size=Some((m.info.rect.w as u32,m.info.rect.h as u32).into()));surface.send_configure();
                if m.info.primary{self.lock.surface=Some(surface.clone());}
            }
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
        let Some(name)=self.monitors.iter().find(|m|m.output.owns(&output)).map(|m|m.info.name.clone())else{return};
        self.lock.extra_surfaces.retain(|(n,_)|*n!=name);
        self.lock.extra_surfaces.push((name,surface));
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
