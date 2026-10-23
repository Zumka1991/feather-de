use std::time::{Duration, Instant};

use smithay::{
    backend::renderer::{
        element::{
            memory::MemoryRenderBufferRenderElement,
            render_elements,
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            utils::RescaleRenderElement,
            Element, Kind,
        },
        gles::GlesRenderer,
        Color32F,
    },
    desktop::{PopupManager, WindowSurfaceType},
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Physical, Point, Scale},
    wayland::compositor::{with_surface_tree_downward, SurfaceAttributes, TraversalAction},
};

use crate::{
    shell::MacShell,
    theme::{to_phys, ui_scale},
    window::{BuiltinContext, ManagedWindow},
    FeatherCompositor,
};

/// Per-frame timings, enabled with FEATHER_PROFILE=1 and printed every 2 seconds
#[derive(Default)]
pub struct FrameStats {
    window_start: Option<Instant>,
    renders: u32,
    submits: u32,
    commits: u32,
    cpu_ms: f64,
    gpu_ms: f64,
    swap_ms: f64,
    worst_ms: f64,
    /// Key press → first submitted frame after the focused client reacted
    pub key_pressed_at: Option<Instant>,
    pub key_answered: bool,
    key_latency_ms: Vec<f64>,
}

impl FrameStats {
    pub fn record_commit(&mut self) {
        self.commits += 1;
        if self.key_pressed_at.is_some() {
            self.key_answered = true;
        }
    }

    pub fn record_key(&mut self) {
        self.key_pressed_at = Some(Instant::now());
        self.key_answered = false;
    }

    fn record(&mut self, cpu: Duration, gpu: Duration, swap: Option<Duration>) {
        let start = *self.window_start.get_or_insert_with(Instant::now);
        self.renders += 1;
        self.cpu_ms += cpu.as_secs_f64() * 1e3;
        self.gpu_ms += gpu.as_secs_f64() * 1e3;
        let total = (cpu + gpu).as_secs_f64() * 1e3;
        if let Some(swap) = swap {
            self.submits += 1;
            self.swap_ms += swap.as_secs_f64() * 1e3;
            if self.key_answered {
                if let Some(t) = self.key_pressed_at.take() {
                    self.key_latency_ms.push(t.elapsed().as_secs_f64() * 1e3);
                }
                self.key_answered = false;
            }
        }
        self.worst_ms = self.worst_ms.max(total);
        let elapsed = start.elapsed().as_secs_f64();
        if elapsed >= 2.0 {
            let n = self.renders.max(1) as f64;
            eprintln!(
                "[feather-profile] кадров {:.0}/с (с изменениями {:.0}/с), коммитов {:.0}/с | CPU-буферы {:.2} мс, сцена+GL {:.2} мс, худший {:.2} мс, swap {:.2} мс",
                self.renders as f64 / elapsed,
                self.submits as f64 / elapsed,
                self.commits as f64 / elapsed,
                self.cpu_ms / n,
                self.gpu_ms / n,
                self.worst_ms,
                self.swap_ms / self.submits.max(1) as f64,
            );
            if !self.key_latency_ms.is_empty() {
                let n = self.key_latency_ms.len() as f64;
                let avg = self.key_latency_ms.iter().sum::<f64>() / n;
                let max = self.key_latency_ms.iter().cloned().fold(0.0, f64::max);
                eprintln!(
                    "[feather-profile] клавиша → кадр (до отправки хосту): среднее {avg:.1} мс, худшее {max:.1} мс, нажатий {}",
                    self.key_latency_ms.len()
                );
            }
            *self = FrameStats::default();
        }
    }
}

render_elements! {
    pub FeatherElement<=GlesRenderer>;
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
    Surface=RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    Rounded=crate::rounded::RoundedSurfaceElement,
}

const CLEAR_COLOR: Color32F = Color32F::new(0.06, 0.07, 0.12, 1.0);
/// Frame callbacks of idle (no-damage) frames are paced to roughly 60 Hz
const IDLE_FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// If the host stops sending frame events (window hidden), don't stall forever
const HOST_FRAME_TIMEOUT: Duration = Duration::from_millis(40);

/// Wayland surface elements are produced at scale 1.0 (logical size at a physical location) and
/// rescaled around their own origin, so the whole scene renders with a single damage tracker
/// at scale 1.0 next to our physical-pixel shell buffers.
fn push_surface_tree(
    out: &mut Vec<FeatherElement>,
    renderer: &mut GlesRenderer,
    surface: &WlSurface,
    location: Point<i32, Physical>,
    scale: f64,
) {
    push_window_surfaces(out, renderer, surface, location, scale, None);
}

/// Texture sizes (pixels) of every surface in a tree, keyed by render element id; surfaces
/// with rotated/flipped buffers are left out (the corner shader assumes a normal transform)
fn surface_tex_sizes(surface: &WlSurface) -> Vec<(smithay::backend::renderer::element::Id, smithay::utils::Size<i32, smithay::utils::Buffer>)> {
    use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
    let mut out = Vec::new();
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |s, states, &()| {
            if let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() {
                let data = data.lock().unwrap();
                if data.buffer_transform() == smithay::utils::Transform::Normal {
                    if let Some(size) = data.buffer_size() {
                        let scale = data.buffer_scale();
                        out.push((
                            smithay::backend::renderer::element::Id::from_wayland_resource(s),
                            (size.w * scale, size.h * scale).into(),
                        ));
                    }
                }
            }
        },
        |_, _, &()| true,
    );
    out
}

/// Surface tree of a window; with `clip`, surfaces get GPU-rounded corners
fn push_window_surfaces(
    out: &mut Vec<FeatherElement>,
    renderer: &mut GlesRenderer,
    surface: &WlSurface,
    location: Point<i32, Physical>,
    scale: f64,
    clip: Option<(crate::rounded::Clip, &smithay::backend::renderer::gles::GlesTexProgram)>,
) {
    let elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        render_elements_from_surface_tree(renderer, surface, location, scale, 1.0, Kind::Unspecified);
    let sizes = if clip.is_some() { surface_tex_sizes(surface) } else { Vec::new() };
    for elem in elems {
        let origin = elem.geometry(Scale::from(1.0)).loc;
        let tex = sizes.iter().find(|(id, _)| id == elem.id()).map(|(_, s)| *s);
        let rescaled = RescaleRenderElement::from_element(elem, origin, scale);
        match (clip, tex) {
            (Some((clip, program)), Some(tex)) => out.push(FeatherElement::Rounded(
                crate::rounded::RoundedSurfaceElement::new(rescaled, program.clone(), clip, tex),
            )),
            _ => out.push(FeatherElement::Surface(rescaled)),
        }
    }
}

/// Pointer image for the DRM backend (front-most elements)
fn cursor_elements(
    renderer: &mut GlesRenderer,
    cursor: &mut crate::cursor::CursorManager,
    pointer: (f64, f64),
    scale: f64,
) -> Vec<FeatherElement> {
    use smithay::input::pointer::CursorImageStatus;
    let mut out = Vec::new();
    let px = ((pointer.0 * scale).round() as i32, (pointer.1 * scale).round() as i32);
    match cursor.status.clone() {
        CursorImageStatus::Hidden => {}
        CursorImageStatus::Named(icon) => {
            if let Some((buf, hot)) = cursor.named(icon, scale) {
                if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    ((px.0 - hot.x) as f64, (px.1 - hot.y) as f64),
                    buf,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ) {
                    out.push(FeatherElement::Memory(e));
                }
            }
        }
        CursorImageStatus::Surface(surface) => {
            if smithay::utils::IsAlive::alive(&surface) {
                let hot = crate::cursor::surface_hotspot(&surface);
                let loc = Point::from((
                    px.0 - (hot.x as f64 * scale).round() as i32,
                    px.1 - (hot.y as f64 * scale).round() as i32,
                ));
                let elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    render_elements_from_surface_tree(renderer, &surface, loc, scale, 1.0, Kind::Cursor);
                for elem in elems {
                    let origin = elem.geometry(Scale::from(1.0)).loc;
                    out.push(FeatherElement::Surface(RescaleRenderElement::from_element(elem, origin, scale)));
                }
            } else {
                cursor.status = CursorImageStatus::default_named();
            }
        }
    }
    out
}

fn to_phys_point(x: i32, y: i32, scale: f64) -> Point<i32, Physical> {
    Point::from(((x as f64 * scale).round() as i32, (y as f64 * scale).round() as i32))
}

/// Builds the scene front-to-back (first element is on top), as the damage tracker expects.
fn build_scene(
    renderer: &mut GlesRenderer,
    shell: &MacShell,
    windows: &[ManagedWindow],
    scale: f64,
    pointer: (f64, f64),
    rounded: Option<&smithay::backend::renderer::gles::GlesTexProgram>,
) -> Vec<FeatherElement> {
    let mut out: Vec<FeatherElement> = Vec::with_capacity(64);
    let mut mem = |out: &mut Vec<FeatherElement>, loc: (f64, f64), buf, size| {
        if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            loc,
            buf,
            None,
            None,
            size,
            Kind::Unspecified,
        ) {
            out.push(FeatherElement::Memory(e));
        }
    };

    let fullscreen_top = windows
        .iter()
        .rev()
        .find(|w| w.is_visible())
        .is_some_and(|w| w.fullscreen);

    if shell.ghost.is_some() {
        let half = crate::shell::GHOST_SIZE as f64 * 0.5;
        mem(
            &mut out,
            (((pointer.0 - half) * scale).round(), ((pointer.1 - half) * scale).round()),
            &shell.ghost_buffer,
            None,
        );
    }
    if shell.switcher.as_ref().is_some_and(|s| s.visible) {
        let r = shell.switcher_rect;
        mem(
            &mut out,
            ((r.x as f64 * scale).round(), (r.y as f64 * scale).round()),
            &shell.switcher_buffer,
            None,
        );
    }
    if shell.has_visible_overlay() {
        mem(&mut out, (0.0, 0.0), &shell.overlay_buffer, None);
    }
    let launchpad_open = shell.launchpad.is_some();
    if !fullscreen_top && !launchpad_open {
        mem(&mut out, (0.0, 0.0), &shell.topbar_buffer, None);
    }
    if !fullscreen_top || launchpad_open {
        let dock_x = (((shell.screen_w - shell.dock_buf_w) as f64) * 0.5 * scale).round();
        let dock_y = (((shell.screen_h - shell.dock_buf_h) as f64) * scale).round();
        mem(&mut out, (dock_x, dock_y), &shell.dock_buffer, None);
    }
    if let Some(lp) = shell.launchpad.as_ref() {
        for ((x, y), buf) in lp.elements() {
            mem(
                &mut out,
                ((x as f64 * scale).round(), (y as f64 * scale).round()),
                buf,
                None,
            );
        }
    }
    if launchpad_open {
        // Opaque frosted wallpaper hides every window: nothing below needs drawing
        let size = (to_phys(shell.screen_w), to_phys(shell.screen_h));
        let src = shell
            .launchpad_bg_key
            .map(|(_, w, h, _)| smithay::utils::Rectangle::from_size((w as f64, h as f64).into()));
        if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (0.0, 0.0),
            &shell.launchpad_bg,
            None,
            src,
            Some(size.into()),
            Kind::Unspecified,
        ) {
            out.push(FeatherElement::Memory(e));
        }
        return out;
    }
    drop(mem);

    for win in windows.iter().rev() {
        if !win.is_visible() {
            continue;
        }

        if let Some(surface) = win.wl_surface() {
            let origin = win.surface_origin();
            // Popups (menus, dropdowns, tooltips) sit above their toplevel
            for (popup, popup_offset) in PopupManager::popups_for_surface(surface) {
                let loc = origin + win.geo_loc + popup_offset - popup.geometry().loc;
                push_surface_tree(
                    &mut out,
                    renderer,
                    popup.wl_surface(),
                    to_phys_point(loc.x, loc.y, scale),
                    scale,
                );
            }
            // macOS corners: bottom ones under our titlebar, all four for square CSD apps
            let clip = rounded.and_then(|program| {
                let round = !win.maximized
                    && !win.fullscreen
                    && (win.has_titlebar() || !win.client_shadow);
                round.then(|| {
                    let (cx, cy) = win.client_origin();
                    let geo = smithay::utils::Rectangle::new(
                        to_phys_point(cx, cy, scale),
                        (
                            (win.width as f64 * scale).round() as i32,
                            (win.height as f64 * scale).round() as i32,
                        )
                            .into(),
                    );
                    let clip = crate::rounded::Clip {
                        geo,
                        radius: crate::theme::WINDOW_CORNER_RADIUS * scale as f32,
                        all_corners: !win.has_titlebar(),
                    };
                    (clip, program)
                })
            });
            push_window_surfaces(
                &mut out,
                renderer,
                surface,
                to_phys_point(origin.x, origin.y, scale),
                scale,
                clip,
            );
        }

        if win.has_titlebar() {
            if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                ((win.x as f64 * scale).round(), (win.y as f64 * scale).round()),
                &win.decoration_buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            ) {
                out.push(FeatherElement::Memory(e));
            }
        }

        if win.draws_shadow() {
            out.extend(
                win.shadow
                    .elements(renderer, win.frame_rect(), scale)
                    .into_iter()
                    .map(FeatherElement::Memory),
            );
        }

        if win.fullscreen {
            // Nothing below a fullscreen window is visible
            return out;
        }
    }

    let wp_size = (to_phys(shell.screen_w), to_phys(shell.screen_h));
    // The source rect must be the (low-res) buffer itself; by default Smithay would use the
    // destination size and sample outside the texture
    let (bw, bh) = crate::shell::wallpaper_buffer_size(shell.screen_w, shell.screen_h);
    if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        (0.0, 0.0),
        &shell.wallpaper_buffer,
        None,
        Some(smithay::utils::Rectangle::from_size((bw as f64, bh as f64).into())),
        Some(wp_size.into()),
        Kind::Unspecified,
    ) {
        out.push(FeatherElement::Memory(e));
    }
    out
}

/// Reads back our own output (not the host screen) into a PNG for visual tests
fn save_screenshot(
    renderer: &mut GlesRenderer,
    fb: &<GlesRenderer as smithay::backend::renderer::RendererSuper>::Framebuffer<'_>,
    w: i32,
    h: i32,
    path: &std::path::Path,
) {
    use smithay::backend::{allocator::Fourcc, renderer::ExportMem};
    let region = smithay::utils::Rectangle::from_size((w, h).into());
    let saved = renderer
        .copy_framebuffer(fb, region, Fourcc::Abgr8888)
        .ok()
        .and_then(|map| renderer.map_texture(&map).ok().map(|b| b.to_vec()))
        .and_then(|mut data| {
            // GL reads rows bottom-up
            let stride = w as usize * 4;
            let rows: Vec<Vec<u8>> = data.chunks_exact(stride).map(|r| r.to_vec()).collect();
            data.clear();
            for r in rows.into_iter().rev() {
                data.extend_from_slice(&r);
            }
            tiny_skia::Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(w as u32, h as u32)?)
        })
        .map(|pm| pm.save_png(path).is_ok())
        .unwrap_or(false);
    tracing::info!("Скриншот {} → {}", path.display(), if saved { "сохранён" } else { "ошибка" });
}

pub fn send_frames_surface_tree(surface: &WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surf, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}

impl FeatherCompositor {
    /// Rasterizes dirty shell/decoration buffers on the CPU (only when something changed)
    fn update_cpu_buffers(&mut self) {
        let active_title = self.active_window_title();
        self.shell.ensure_glass_bg();
        self.shell.ensure_wallpaper_rendered();
        self.shell
            .ensure_topbar_rendered(&mut self.fonts, &active_title, &self.active_layout_name);
        if self.dock_model_dirty {
            self.dock_model_dirty = false;
            let items = self.build_dock_items();
            self.shell.set_dock_items(items);
            let ctx = self.build_menu_context();
            self.shell.set_menu_context(ctx);
        }
        self.shell.ensure_dock_rendered(&mut self.fonts);
        self.shell.ensure_switcher_rendered(&mut self.fonts);
        self.shell.ensure_ghost_rendered(&mut self.fonts);
        if self.shell.launchpad.is_some() {
            self.shell.ensure_launchpad_bg();
        }
        if let Some(lp) = self.shell.launchpad.as_mut() {
            lp.ensure_rendered(&mut self.fonts, &mut self.shell.icons, &self.app_registry);
            if !lp.prefetch_scheduled && !lp.missing_neighbour_tiles().is_empty() {
                lp.prefetch_scheduled = true;
                self.loop_handle.insert_idle(|state| state.launchpad_prefetch_step());
            }
        }
        if self.shell.has_visible_overlay() {
            self.shell
                .ensure_overlay_rendered(&mut self.fonts, &self.app_registry, &self.active_layout_name);
        }

        let visible_count = self.windows.iter().filter(|w| w.is_visible()).count();
        let ctx = BuiltinContext {
            notes_text: &self.notes_buffer,
            rss_mb: self.shell.last_rss_mb,
            window_count: visible_count,
            wayland_socket: &self.wayland_socket_name,
            active_layout: &self.active_layout_name,
            theme: self.shell.theme,
            dock_zoom_enabled: self.shell.dock_zoom_enabled,
            reload_count: self.shell.reload_count,
            scale_factor: ui_scale(),
        };
        for win in &mut self.windows {
            if win.is_visible() {
                let focused = win.activated;
                win.ensure_rendered(&mut self.fonts, focused, &ctx);
            }
        }
    }

    /// Rasterizes a few Launchpad tiles of the neighbouring pages while the loop is idle, so
    /// flipping pages never waits for icon rasterization
    pub fn launchpad_prefetch_step(&mut self) {
        let Some(lp) = self.shell.launchpad.as_mut() else { return };
        let more = lp.prefetch(2, &mut self.fonts, &mut self.shell.icons, &self.app_registry);
        if more {
            self.loop_handle.insert_idle(|state| state.launchpad_prefetch_step());
        } else {
            lp.prefetch_scheduled = false;
        }
    }

    pub fn render_if_needed(&mut self) {
        // One frame in flight at a time: the next one is rendered when the previous one
        // reached the screen (host frame event / DRM vblank), so a burst of input or commits
        // never blocks the loop while a client frame is waiting.
        if !self.needs_redraw || self.frame_pending {
            return;
        }
        if let crate::backend::Backend::Drm(d) = &self.backend {
            if !d.active {
                // Switched to another VT: render again once the session comes back
                return;
            }
        }
        self.needs_redraw = false;
        let t_cpu = Instant::now();
        self.update_cpu_buffers();
        let cpu_time = t_cpu.elapsed();
        let t_gpu = Instant::now();

        let presented = if self.backend.is_drm() {
            self.render_drm()
        } else {
            self.render_winit()
        };

        let gpu_time = t_gpu.elapsed();
        let swap_time = match presented {
            Some(swap) => {
                self.mark_frame_in_flight();
                // Clients start their next frame while ours is being presented
                self.send_frame_callbacks();
                Some(swap)
            }
            None => {
                self.schedule_idle_frame_callbacks();
                None
            }
        };
        if let Some(stats) = self.frame_stats.as_mut() {
            stats.record(cpu_time, gpu_time, swap_time);
        }
    }

    /// Nested: render into the host window; returns the swap duration if a frame was submitted
    fn render_winit(&mut self) -> Option<Duration> {
        let crate::backend::Backend::Winit(backend) = &mut self.backend else {
            return None;
        };
        let scale = self.scale_factor;
        let age = backend.buffer_age().unwrap_or(0);
        let damage = {
            let (renderer, mut framebuffer) = match backend.bind() {
                Ok(b) => b,
                Err(err) => {
                    tracing::warn!("Не удалось привязать буфер вывода: {err:?}");
                    return None;
                }
            };
            if !self.swap_interval_disabled {
                // Mesa defaults to interval 1, making eglSwapBuffers block the whole event
                // loop until vblank. We pace frames ourselves via host frame callbacks.
                let display = renderer.egl_context().display().get_display_handle();
                unsafe {
                    smithay::backend::egl::ffi::egl::SwapInterval(display.handle, 0);
                }
                self.swap_interval_disabled = true;
            }
            let pointer = (self.pointer_location.x, self.pointer_location.y);
            let elements = build_scene(
                renderer,
                &self.shell,
                &self.windows,
                scale,
                pointer,
                self.rounded_program.as_ref(),
            );
            let result = match self
                .damage_tracker
                .render_output(renderer, &mut framebuffer, age, &elements, CLEAR_COLOR)
            {
                Ok(res) => res.damage.cloned(),
                Err(err) => {
                    tracing::warn!("Ошибка рендера: {err:?}");
                    None
                }
            };
            if let Some(path) = self.screenshot_request.take() {
                save_screenshot(renderer, &framebuffer, self.phys_w, self.phys_h, &path);
            }
            result
        };
        match damage {
            Some(damage) if !damage.is_empty() => {
                let t_swap = Instant::now();
                if let Err(err) = backend.submit(Some(&damage)) {
                    tracing::warn!("Ошибка submit: {err:?}");
                }
                backend.window().request_redraw();
                Some(t_swap.elapsed())
            }
            _ => None,
        }
    }

    /// Direct DRM: compose (or scan out directly) and queue the frame for the next vblank
    fn render_drm(&mut self) -> Option<Duration> {
        let crate::backend::Backend::Drm(d) = &mut self.backend else {
            return None;
        };
        let scale = self.scale_factor;
        let pointer = (self.pointer_location.x, self.pointer_location.y);
        let mut elements = cursor_elements(&mut d.renderer, &mut self.cursor, pointer, scale);
        elements.extend(build_scene(
            &mut d.renderer,
            &self.shell,
            &self.windows,
            scale,
            pointer,
            self.rounded_program.as_ref(),
        ));

        let t = Instant::now();
        match d.output.render_frame(
            &mut d.renderer,
            &elements,
            CLEAR_COLOR,
            smithay::backend::drm::compositor::FrameFlags::DEFAULT,
        ) {
            Ok(res) if !res.is_empty => {
                if let Err(err) = d.output.queue_frame(()) {
                    tracing::warn!("DRM queue_frame: {err:?}");
                    return None;
                }
                Some(t.elapsed())
            }
            Ok(_) => None,
            Err(err) => {
                tracing::warn!("DRM render_frame: {err:?}");
                None
            }
        }
    }

    fn mark_frame_in_flight(&mut self) {
        self.frame_pending = true;
        self.frame_seq = self.frame_seq.wrapping_add(1);
        if self.backend.is_drm() {
            // DRM always delivers a vblank event for a queued frame
            return;
        }
        let seq = self.frame_seq;
        let _ = self
            .loop_handle
            .insert_source(Timer::from_duration(HOST_FRAME_TIMEOUT), move |_, _, state| {
                if state.frame_pending && state.frame_seq == seq {
                    state.frame_pending = false;
                }
                TimeoutAction::Drop
            });
    }

    /// The host compositor presented our last frame: the next one may be rendered
    pub fn on_host_frame(&mut self) {
        self.frame_pending = false;
    }

    pub fn send_frame_callbacks(&mut self) {
        let time = self.start_time.elapsed().as_millis() as u32;
        if let smithay::input::pointer::CursorImageStatus::Surface(ref s) = self.cursor.status {
            send_frames_surface_tree(s, time);
        }
        for win in &self.windows {
            if let Some(surface) = win.wl_surface() {
                send_frames_surface_tree(surface, time);
                for (popup, _) in PopupManager::popups_for_surface(surface) {
                    send_frames_surface_tree(popup.wl_surface(), time);
                }
            }
        }
    }

    /// A frame without damage does not block on vsync. Answering frame callbacks right away
    /// would let a client that commits without changes spin us at 100% CPU, so delay them.
    fn schedule_idle_frame_callbacks(&mut self) {
        if self.idle_frame_timer_armed {
            return;
        }
        self.idle_frame_timer_armed = true;
        let res = self
            .loop_handle
            .insert_source(Timer::from_duration(IDLE_FRAME_INTERVAL), |_, _, state| {
                state.idle_frame_timer_armed = false;
                state.send_frame_callbacks();
                TimeoutAction::Drop
            });
        if res.is_err() {
            self.idle_frame_timer_armed = false;
            self.send_frame_callbacks();
        }
    }

    /// Topmost visible surface (popups first) under a logical point
    pub fn surface_under_pointer(
        &self,
        px: f64,
        py: f64,
    ) -> Option<(WlSurface, Point<f64, smithay::utils::Logical>)> {
        if self.shell.launchpad.is_some() || self.icon_drag.as_ref().is_some_and(|d| d.active) {
            // Launchpad covers all windows; while dragging an app nothing below gets the pointer
            return None;
        }
        let pos = Point::<f64, smithay::utils::Logical>::from((px, py));
        for win in self.windows.iter().rev() {
            if !win.is_visible() {
                continue;
            }
            let Some(surface) = win.wl_surface() else {
                if win.contains_point(px, py) {
                    return None;
                }
                continue;
            };
            let origin = win.surface_origin();
            for (popup, popup_offset) in PopupManager::popups_for_surface(surface) {
                let loc = origin + win.geo_loc + popup_offset - popup.geometry().loc;
                if let Some((surf, sub_loc)) = smithay::desktop::utils::under_from_surface_tree(
                    popup.wl_surface(),
                    // Global point: the helper subtracts `loc` itself
                    pos,
                    loc,
                    WindowSurfaceType::ALL,
                ) {
                    return Some((surf, sub_loc.to_f64()));
                }
            }
            if win.contains_point(px, py) || win.client_shadow {
                if let Some((surf, sub_loc)) = smithay::desktop::utils::under_from_surface_tree(
                    surface,
                    pos,
                    origin,
                    WindowSurfaceType::ALL,
                ) {
                    return Some((surf, sub_loc.to_f64()));
                }
            }
            if win.contains_point(px, py) {
                return None;
            }
        }
        None
    }
}
