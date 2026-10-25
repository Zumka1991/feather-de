use std::time::{Duration, Instant};

use smithay::{
    backend::renderer::{
        element::{
            memory::MemoryRenderBufferRenderElement,
            render_elements,
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            utils::{Relocate, RelocateRenderElement, RescaleRenderElement},
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
    animation: Option<AnimationStats>,
}

#[derive(Default)]
struct AnimationStats {
    frames: u32,
    first_cpu_ms: f64,
    cpu_ms: f64,
    gpu_ms: f64,
    worst_gpu_ms: f64,
    start: Option<Instant>,
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

    fn record_animation(&mut self, active: bool, cpu: Duration, gpu: Duration) {
        if !active && self.animation.is_none() { return; }
        let stats = self.animation.get_or_insert_with(AnimationStats::default);
        stats.start.get_or_insert_with(Instant::now);
        if stats.frames == 0 { stats.first_cpu_ms = cpu.as_secs_f64() * 1e3; }
        else { stats.cpu_ms += cpu.as_secs_f64() * 1e3; }
        stats.frames += 1;
        stats.gpu_ms += gpu.as_secs_f64() * 1e3;
        stats.worst_gpu_ms = stats.worst_gpu_ms.max(gpu.as_secs_f64() * 1e3);
        if !active {
            let stats = self.animation.take().unwrap();
            let n = stats.frames as f64;
            let elapsed = stats.start.unwrap().elapsed().as_secs_f64();
            eprintln!("[feather-profile] анимация: {} кадров, {:.0}/с | CPU первый {:.2} мс, следующие {:.3} мс | сцена+GL {:.2} мс, худший {:.2} мс | пружины остановлены",
                stats.frames, (n - 1.0) / elapsed.max(0.001), stats.first_cpu_ms,
                stats.cpu_ms / (n - 1.0).max(1.0), stats.gpu_ms / n, stats.worst_gpu_ms);
        }
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
            let animation = self.animation.take();
            *self = FrameStats::default();
            self.animation = animation;
        }
    }
}

render_elements! {
    pub SceneElement<=GlesRenderer>;
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
    Surface=RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    Rounded=crate::rounded::RoundedSurfaceElement,
    Texture=smithay::backend::renderer::element::texture::TextureRenderElement<smithay::backend::renderer::gles::GlesTexture>,
}

render_elements! {
    pub FeatherElement<=GlesRenderer>;
    Animated=RelocateRenderElement<RescaleRenderElement<SceneElement>>,
}

render_elements! {
    pub OutputElement<=GlesRenderer>;
    View=RelocateRenderElement<FeatherElement>,
}
fn output_elements(elements:Vec<FeatherElement>,rect:crate::monitors::Rect,scale:f64)->Vec<OutputElement>{
    let offset=(-(rect.x as f64*scale).round() as i32,-(rect.y as f64*scale).round() as i32);
    elements.into_iter().map(|e|OutputElement::View(RelocateRenderElement::from_element(e,offset,Relocate::Relative))).collect()
}

impl crate::animation::Visual {
    fn element(self, element: SceneElement, scale: f64) -> FeatherElement {
        let pivot = ((self.center.0 * scale).round() as i32, (self.center.1 * scale).round() as i32).into();
        let offset = ((self.offset.0 * scale).round() as i32, (self.offset.1 * scale).round() as i32);
        FeatherElement::Animated(RelocateRenderElement::from_element(
            RescaleRenderElement::from_element(element, pivot, self.scale), offset, Relocate::Relative,
        ))
    }
}

impl From<SceneElement> for FeatherElement {
    fn from(element: SceneElement) -> Self {
        crate::animation::Visual::default().element(element, 1.0)
    }
}

pub(crate) const CLEAR_COLOR: Color32F = Color32F::new(0.06, 0.07, 0.12, 1.0);
/// Frame callbacks of idle (no-damage) frames are paced to roughly 60 Hz
const IDLE_FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// If the host stops sending frame events (window hidden), don't stall forever
const HOST_FRAME_TIMEOUT: Duration = Duration::from_millis(40);

/// Wayland surface elements are produced at scale 1.0 (logical size at a physical location) and
/// rescaled around their own origin, so the whole scene renders with a single damage tracker
/// at scale 1.0 next to our physical-pixel shell buffers.
pub(crate) fn push_surface_tree(
    out: &mut Vec<FeatherElement>,
    renderer: &mut GlesRenderer,
    surface: &WlSurface,
    location: Point<i32, Physical>,
    scale: f64,
    visual: crate::animation::Visual,
) {
    push_window_surfaces(out, renderer, surface, location, scale, None, visual);
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
    visual: crate::animation::Visual,
) {
    let elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        render_elements_from_surface_tree(renderer, surface, location, scale, visual.alpha, Kind::Unspecified);
    let sizes = if clip.is_some() { surface_tex_sizes(surface) } else { Vec::new() };
    for elem in elems {
        let origin = elem.geometry(Scale::from(1.0)).loc;
        let tex = sizes.iter().find(|(id, _)| id == elem.id()).map(|(_, s)| *s);
        let rescaled = RescaleRenderElement::from_element(elem, origin, scale);
        let element = match (clip, tex) {
            (Some((clip, program)), Some(tex)) => SceneElement::Rounded(
                crate::rounded::RoundedSurfaceElement::new(rescaled, program.clone(), clip, tex),
            ),
            _ => SceneElement::Surface(rescaled),
        };
        out.push(visual.element(element, scale));
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
                    out.push(SceneElement::Memory(e).into());
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
                    out.push(SceneElement::Surface(RescaleRenderElement::from_element(elem, origin, scale)).into());
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
pub(crate) fn build_scene(
    renderer: &mut GlesRenderer,
    shell: &MacShell,
    windows: &[ManagedWindow],
    scale: f64,
    pointer: (f64, f64),
    rounded: Option<&smithay::backend::renderer::gles::GlesTexProgram>,
    animations: &crate::animation::Animations,
    view: crate::monitors::Rect,
) -> Vec<FeatherElement> {
    let mut out: Vec<FeatherElement> = Vec::with_capacity(64);
    let primary=view.x==0&&view.y==0;
    if primary {push_space_thumbnails(&mut out, renderer, shell, windows, scale, rounded, animations);}
    let mut mem = |out: &mut Vec<FeatherElement>, loc: (f64, f64), buf, src, visual: crate::animation::Visual| {
        if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            loc,
            buf,
            Some(visual.alpha),
            src,
            None,
            Kind::Unspecified,
        ) {
            out.push(visual.element(SceneElement::Memory(e), scale));
        }
    };

    let fullscreen_top = windows
        .iter()
        .rev()
        .find(|w| w.is_visible() && crate::monitors::Rect{x:w.x,y:w.y,w:w.width,h:w.total_height()}.intersection(view)>0)
        .is_some_and(|w| w.fullscreen && animations.window(w).identity());

    if primary {
    if shell.ghost.is_some() {
        let half = crate::shell::GHOST_SIZE as f64 * 0.5;
        mem(
            &mut out,
            (((pointer.0 - half) * scale).round(), ((pointer.1 - half) * scale).round()),
            &shell.ghost_buffer,
            None,
            crate::animation::Visual::default(),
        );
    }
    if shell.switcher.as_ref().is_some_and(|s| s.visible) {
        let r = shell.switcher_rect;
        mem(
            &mut out,
            ((r.x as f64 * scale).round(), (r.y as f64 * scale).round()),
            &shell.switcher_buffer,
            None,
            crate::animation::Visual::default(),
        );
    }
    if shell.has_visible_overlay() {
        if let Some(r) = shell.overlay_region() {
            mem(&mut out, (r.loc.x as f64, r.loc.y as f64), &shell.overlay_buffer,
                None, animations.overlay(shell));
        }
    }
    let launchpad_open = shell.launchpad.is_some();
    if !fullscreen_top && !launchpad_open {
        mem(&mut out, (0.0, 0.0), &shell.topbar_buffer, None, crate::animation::Visual::default());
    }
    if !fullscreen_top || launchpad_open {
        use crate::theme::DockEdge;
        let edge = crate::theme::dock_edge();
        let (sx, sy) = shell.dock_shown_origin();
        // Auto-hide: the bar slides beyond its screen edge (GPU offset of the same buffer)
        let hide = animations.dock_hidden() * ((crate::theme::dock_icon() + 24.0) as f64 + 8.0);
        let (hx, hy) = match edge { DockEdge::Bottom => (0.0, hide), DockEdge::Left => (-hide, 0.0), DockEdge::Right => (hide, 0.0) };
        let (dock_x, dock_y) = (((sx as f64 + hx) * scale).round(), ((sy as f64 + hy) * scale).round());
        if let Some(b) = shell.bounce.as_ref() {
            if let Some((x, y, size)) = shell.dock_slot_global(b.target) {
                let (lift, _) = b.offset(Instant::now());
                // Centred along the bar, resting on the edge side, jumping away from the edge
                let inset = (size - crate::theme::dock_icon()) as f64 * 0.5;
                let lift = lift as f64;
                let (x, y) = (x as f64, y as f64);
                let (lx, ly) = match edge {
                    DockEdge::Bottom => (x + inset, y + inset * 2.0 - lift),
                    DockEdge::Left => (x + lift, y + inset),
                    DockEdge::Right => (x + inset * 2.0 - lift, y + inset),
                };
                let src = smithay::utils::Rectangle::new((0.0, 0.0).into(), (b.px as f64, b.px as f64).into());
                mem(&mut out, ((lx * scale).round(), (ly * scale).round()), &b.buffer, Some(src), crate::animation::Visual::default());
            }
        }
        mem(&mut out, (dock_x, dock_y), &shell.dock_buffer, None, crate::animation::Visual::default());
    }
    if let Some(lp) = shell.launchpad.as_ref() {
        for ((x, y), buf) in lp.elements() {
            mem(
                &mut out,
                ((x as f64 * scale).round(), (y as f64 * scale).round()),
                buf,
                None,
                animations.launchpad(shell),
            );
        }
    }
    }
    drop(mem);
    let launchpad_open=primary && shell.launchpad.is_some();
    if launchpad_open && shell.launchpad_bg_key.is_some() {
        // Opaque frosted wallpaper hides every window: nothing below needs drawing
        let size = (to_phys(shell.screen_w), to_phys(shell.screen_h));
        let src = shell
            .launchpad_bg_key
            .map(|(_, w, h, _)| smithay::utils::Rectangle::from_size((w as f64, h as f64).into()));
        if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (0.0, 0.0),
            &shell.launchpad_bg,
            Some(animations.launchpad(shell).alpha),
            src,
            Some(size.into()),
            Kind::Unspecified,
        ) {
            out.push(SceneElement::Memory(e).into());
        }
        if animations.launchpad(shell).identity() { return out; }
    }

    for win in windows.iter().rev() {
        if !animations.draws_window(win) || (animations.window(win).identity() && crate::monitors::Rect{x:win.x,y:win.y,w:win.width,h:win.total_height()}.intersection(view)==0) {
            continue;
        }
        let visual = animations.window(win);
        push_window(&mut out, renderer, win, visual, scale, rounded, true);

        if win.fullscreen && !win.minimized && visual.identity() && win.x<=view.x && win.y<=view.y && win.x+win.width>=view.x+view.w && win.y+win.total_height()>=view.y+view.h {
            // Nothing below a fullscreen window is visible
            return out;
        }
    }

    // Mission Control dims the desktop behind the spread windows
    let dim = if primary {animations.mission_progress()}else{0.0};
    if dim > 0.0 {
        if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
            renderer, (0.0, 0.0), &shell.mission_dim, Some((0.35 * dim) as f32),
            Some(smithay::utils::Rectangle::from_size((4.0, 4.0).into())),
            Some((to_phys(shell.screen_w), to_phys(shell.screen_h)).into()), Kind::Unspecified,
        ) {
            out.push(SceneElement::Memory(e).into());
        }
    }
    if primary {out.extend(shell.desktop.elements(renderer, (shell.screen_w, shell.screen_h), scale));}
    let wp_size = ((view.w as f64*scale).round() as i32,(view.h as f64*scale).round() as i32);
    // The source rect must be the (low-res) buffer itself; by default Smithay would use the
    // destination size and sample outside the texture
    if let Some(texture)=&shell.wallpaper_texture{
        let (w,h)=shell.wallpaper_texture_size;
        out.push(SceneElement::Texture(smithay::backend::renderer::element::texture::TextureRenderElement::from_texture_buffer((view.x as f64*scale,view.y as f64*scale),texture,None,Some(smithay::utils::Rectangle::from_size((w as f64,h as f64).into())),Some(wp_size.into()),Kind::Unspecified)).into());
    }
    out
}

/// One window: popups, surfaces (rounded corners), our titlebar and, optionally, shadow
fn push_window(
    out: &mut Vec<FeatherElement>,
    renderer: &mut GlesRenderer,
    win: &ManagedWindow,
    visual: crate::animation::Visual,
    scale: f64,
    rounded: Option<&smithay::backend::renderer::gles::GlesTexProgram>,
    with_shadow: bool,
) {
    let mut out = out;

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
                visual,
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
            visual,
        );
    }

    if win.has_titlebar() {
        let old=win.decoration_size();
        let resized=win.app_id=="builtin.settings";
        if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            ((win.x as f64 * scale).round(), (win.y as f64 * scale).round()),
            &win.decoration_buffer,
            Some(visual.alpha),
            resized.then(||smithay::utils::Rectangle::from_size((old.0 as f64,old.1 as f64).into())),
            resized.then(||((win.width as f64*scale).round() as i32,(win.total_height() as f64*scale).round() as i32).into()),
            Kind::Unspecified,
        ) {
            out.push(visual.element(SceneElement::Memory(e), scale));
        }
    }

    if with_shadow && win.draws_shadow() {
        out.extend(
            win.shadow
                .elements(renderer, win.frame_rect(), scale, visual.alpha)
                .into_iter()
                .map(|e| visual.element(SceneElement::Memory(e), scale)),
        );
    }

}

/// Mission Control: windows of the other desktops, scaled into their strip tiles. The
/// current desktop's windows are already in the overview (drawing them twice would give
/// one element id two places), and desktops in a slide are drawn by the scene itself.
fn push_space_thumbnails(
    out: &mut Vec<FeatherElement>,
    renderer: &mut GlesRenderer,
    shell: &MacShell,
    windows: &[ManagedWindow],
    scale: f64,
    rounded: Option<&smithay::backend::renderer::gles::GlesTexProgram>,
    animations: &crate::animation::Animations,
) {
    let Some(m) = shell.mission.as_ref() else { return };
    let (sw, sh) = (shell.screen_w as f64, shell.screen_h as f64);
    for tile in m.tiles.iter().filter(|t| !t.current) {
        let Some(space) = tile.space.filter(|s| !animations.in_slide(*s)) else { continue };
        let r = tile.rect;
        let k = ((r.w as f64 - 8.0) / sw).min((r.h as f64 - 8.0) / sh);
        let (ox, oy) = (r.x as f64 + (r.w as f64 - sw * k) * 0.5, r.y as f64 + (r.h as f64 - sh * k) * 0.5);
        for win in windows.iter().rev().filter(|w| w.mapped && !w.minimized && w.space == space) {
            let center = (win.x as f64 + win.width as f64 * 0.5, win.y as f64 + win.total_height() as f64 * 0.5);
            let visual = crate::animation::Visual {
                scale: k,
                alpha: animations.mission_progress() as f32,
                center,
                offset: (ox + center.0 * k - center.0, oy + center.1 * k - center.1),
            };
            push_window(out, renderer, win, visual, scale, rounded, false);
        }
    }
}

/// Above everything: dragged desktop icons, or the icon a program gave its drag
fn drag_overlay(out: &mut Vec<FeatherElement>, renderer: &mut GlesRenderer, shell: &MacShell, pointer: (f64, f64), scale: f64) {
    out.extend(shell.desktop.drag_elements((shell.screen_w, shell.screen_h), pointer, scale));
    if let Some(icon) = shell.desktop.dnd_icon.as_ref().filter(|s| smithay::reexports::wayland_server::Resource::is_alive(*s)) {
        let location = to_phys_point(pointer.0 as i32, pointer.1 as i32, scale);
        push_surface_tree(out, renderer, icon, location, scale, crate::animation::Visual::default());
    }
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
        let mut profile = crate::cpu_profile::Batch::new();
        if self.lock.active {self.shell.ensure_wallpaper_rendered();self.lock.paint(&mut self.fonts,self.scale_factor,&self.active_layout_name,(self.shell.screen_w,self.shell.screen_h));profile.step("lock");return;}
        self.audio_hud.paint(&mut self.fonts,self.scale_factor);
        profile.step("audio-hud");
        let active_title = self.active_window_title();
        self.shell.ensure_wallpaper_rendered();
        self.shell.ensure_cc_backdrop();
        if self.dock_model_dirty {
            self.dock_model_dirty = false;
            let items = self.build_dock_items();
            self.shell.set_dock_items(items);
            let ctx = self.build_menu_context();
            self.shell.set_menu_context(ctx);
        }
        profile.step("model");
        self.shell
            .ensure_topbar_rendered(&mut self.fonts, &active_title, &self.active_layout_name);
        profile.step("topbar");
        self.shell.ensure_dock_rendered(&mut self.fonts);
        profile.step("dock");
        self.shell.ensure_switcher_rendered(&mut self.fonts);
        self.shell.ensure_ghost_rendered(&mut self.fonts);
        profile.step("switcher/ghost");
        if self.shell.launchpad.is_some() {
            self.shell.ensure_launchpad_bg();
        }
        if let Some(lp) = self.shell.launchpad.as_mut() {
            lp.ensure_rendered(&mut self.fonts, self.shell.raster.as_ref(), &self.app_registry);
        }
        profile.step("launchpad");
        if self.shell.has_visible_overlay() {
            self.shell
                .ensure_overlay_rendered(&mut self.fonts, &self.app_registry, &self.active_layout_name);
        }

        self.auth.ensure_rendered(&mut self.fonts);
        profile.step("overlay");
        let visible_count = self.windows.iter().filter(|w| w.is_visible()).count();
        let ctx = BuiltinContext {
            settings: &self.system_settings,
            settings_context: crate::system_settings::Context {power:self.power.preferences,power_ready:self.power.ready,system:self.system_preferences.clone(),pointer:self.pointer_config.clone(),pointer_managed:self.pointer_managed,keyboard:&self.keyboard_config,active:self.active_layout_index,animations:self.animations.enabled(),dock_zoom:self.shell.dock_zoom_enabled,scale:self.scale_factor,display_name:&self.display_name,monitors:self.monitors.iter().map(|m|m.info.clone()).collect(),modes:&self.display_modes,current:crate::system_settings::Mode{width:self.phys_w,height:self.phys_h,refresh:self.refresh_mhz},direct:self.backend.is_drm(),wallpaper:self.shell.theme,wallpaper_image:self.shell.wallpaper_image.as_ref(),dnd:self.shell.dnd_active(),dnd_schedule:self.shell.dnd_schedule,notify_sounds:self.shell.notify_sounds,sounds:self.sound_preferences,audio:self.shell.cc.audio.clone(),wifi:self.shell.cc.wifi.clone(),bluetooth:self.shell.cc.bluetooth.clone(),hot_corners:self.hot_corners.actions,night:self.night.prefs,night_active:self.night.active(),notify_apps:&self.shell.notify_apps,sticky_keys:self.sticky_keys},
            notes_text: &self.notes_buffer,
            rss_mb: self.shell.last_rss_mb,
            window_count: visible_count,
            wayland_socket: &self.wayland_socket_name,
            active_layout: &self.active_layout_name,
            input_shortcut: self.keyboard_config.shortcut.label(),
            scale_factor: ui_scale(),
            show_about: self.show_about,
        };
        for win in &mut self.windows {
            if win.is_visible() {
                if win.app_id=="builtin.settings"{
                    if win.dirty {
                        let ticket=std::sync::Arc::new(());
                        let job=crate::system_settings::FrameJob::new(std::sync::Arc::downgrade(&ticket),&self.system_settings,&ctx.settings_context,win);
                        if self.shell.raster.as_ref().is_some_and(|worker|worker.submit(crate::raster::Job::Settings(job))){win.settings_ticket=Some(ticket);win.dirty=false;}
                    }
                    continue;
                }
                let focused = win.activated;
                win.ensure_rendered(&mut self.fonts, focused, &ctx);
            }
        }
        profile.step("windows");
    }

    pub fn render_if_needed(&mut self) {
        // One frame in flight at a time: the next one is rendered when the previous one
        // reached the screen (host frame event / DRM vblank), so a burst of input or commits
        // never blocks the loop while a client frame is waiting.
        if (self.power.blanked && self.backend.is_drm()) || !self.needs_redraw || self.frame_pending {
            return;
        }
        if let crate::backend::Backend::Drm(d) = &self.backend {
            if !d.active {
                // Switched to another VT: render again once the session comes back
                return;
            }
        }
        self.refresh_output_membership();
        self.needs_redraw = false;
        let t_cpu = Instant::now();
        self.shell.release_closed_launchpad();
        self.update_cpu_buffers();
        // The wallpaper is the lock screen's background too
        self.shell.upload_wallpaper(self.backend.renderer());
        if !self.lock.active {
            self.shell.desktop.ensure_layout((self.shell.screen_w, self.shell.screen_h));
            self.shell.desktop.request_tiles(self.shell.raster.as_ref(), &self.app_registry);
            if self.shell.desktop.upload(self.backend.renderer()) {
                if let Some(worker) = self.shell.raster.as_ref() {
                    worker.submit(crate::raster::Job::Reclaim);
                }
            }
            self.needs_redraw = self.animations.advance(&self.windows, &self.shell, Instant::now()) || self.auth.animating();
            self.finish_mission();
            self.advance_bounce();
        }
        let cpu_time = t_cpu.elapsed();
        let t_gpu = Instant::now();

        let presented = if self.backend.is_drm() {
            self.render_drm()
        } else {
            self.render_winit()
        };

        self.shell.reclaim_after_frame();
        let gpu_time = t_gpu.elapsed();
        let swap_time = match presented {
            Some(swap) => {
                if self.lock.active && !self.backend.is_drm() {self.lock.protected_frame=true;}
                self.mark_frame_in_flight();
                // Clients start their next frame while ours is being presented
                self.send_frame_callbacks();
                Some(swap)
            }
            None => {
                if self.lock.active && !self.lock.display_protected {
                    if let crate::backend::Backend::Drm(d)=&mut self.backend {
                        if let Err(e)=d.clear_all(){tracing::error!("Не удалось скрыть вывод при ошибке блокировки: {e}");}
                    }
                }
                if self.lock.active && self.lock.protected_frame && !self.backend.is_drm(){self.lock_presented();}
                if self.lock.active && matches!(&self.backend,crate::backend::Backend::Drm(d) if !d.heads.is_empty()&&d.heads.iter().all(|h|h.protected)){self.lock.protected_frame=true;self.lock_presented();}
                self.schedule_idle_frame_callbacks();
                None
            }
        };
        if let Some(stats) = self.frame_stats.as_mut() {
            stats.record_animation(self.needs_redraw, cpu_time, gpu_time);
            stats.record(cpu_time, gpu_time, swap_time);
        }
    }

    /// Nested: render into the host window; returns the swap duration if a frame was submitted
    fn render_winit(&mut self) -> Option<Duration> {
        let view=self.monitors.iter().find(|m|self.preview_monitor.as_ref()==Some(&m.info.name)).or_else(||self.primary_monitor()).map(|m|m.info.clone())?;

        let crate::backend::Backend::Winit(backend) = &mut self.backend else {
            return None;
        };
        let scale = self.scale_factor;
        // Smithay's target bind resizes the native window but leaves EGL surfaceless.
        // Keep the handle only until submit (which may recreate the EGLSurface).
        let surface = backend.egl_surface().get_surface_handle();
        let damage = {
            let (renderer, mut framebuffer) = match backend.bind() {
                Ok(b) => b,
                Err(err) => {
                    tracing::warn!("Не удалось привязать буфер вывода: {err:?}");
                    return None;
                }
            };
            let display = renderer.egl_context().display().get_display_handle();
            // SAFETY: the backend/context live on this event-loop thread. bind() has
            // applied native resize, and no swap can replace the surface in this scope.
            let current = unsafe {
                smithay::backend::egl::ffi::egl::MakeCurrent(
                    display.handle, surface, surface, renderer.egl_context().get_context_handle(),
                ) == smithay::backend::egl::ffi::egl::TRUE
            };
            if !current {
                tracing::warn!("Не удалось сделать поверхность вывода EGL текущей");
                return None;
            }
            if !self.swap_interval_disabled {
                self.swap_interval_disabled = unsafe {
                    smithay::backend::egl::ffi::egl::SwapInterval(display.handle, 0)
                        == smithay::backend::egl::ffi::egl::TRUE
                };
            }
            // Texture import/readback can unbind the output between frames. Mesa's
            // BUFFER_AGE_EXT query requires the output to be current, even after bind().
            let mut age = 0;
            if renderer.egl_context().display().supports_damage() { unsafe {
                if smithay::backend::egl::ffi::egl::QuerySurface(
                    display.handle, surface, smithay::backend::egl::ffi::egl::BUFFER_AGE_EXT as i32,
                    &mut age,
                ) != smithay::backend::egl::ffi::egl::TRUE {
                    age = 0;
                }
            } }
            let pointer = (self.pointer_location.x, self.pointer_location.y);
            let elements = if self.power.blanked {Vec::new()} else if self.lock.active {
                self.lock.monitor_elements(renderer,scale,&view,self.shell.wallpaper_texture.as_ref().map(|t|(t,self.shell.wallpaper_texture_size)))
            } else {
                let mut elements = if view.primary {self.auth.elements(renderer, scale, (self.shell.screen_w, self.shell.screen_h), (self.phys_w, self.phys_h))}else{Vec::new()};
                if view.primary {elements.extend(self.audio_hud.elements(renderer,scale,(self.shell.screen_w,self.shell.screen_h)));}
                elements.extend(crate::screenshot::overlay(renderer, self.screenshot_select.as_ref(), pointer, scale, (self.phys_w, self.phys_h)));
                drag_overlay(&mut elements, renderer, &self.shell, pointer, scale);
                elements.extend(build_scene(
                    renderer,
                    &self.shell,
                    &self.windows,
                    scale,
                    pointer,
                    self.rounded_program.as_ref(),
                    &self.animations,
                    view.rect,
                ));
                elements
            };
            let elements=if self.lock.active {output_elements(elements,crate::monitors::Rect{x:0,y:0,..view.rect},scale)}else{output_elements(elements,view.rect,scale)};
            let result = match self
                .damage_tracker
                .render_output(renderer, &mut framebuffer, age.max(0) as usize, &elements, if self.power.blanked||self.lock.active {Color32F::new(0.0,0.0,0.0,1.0)}else{CLEAR_COLOR})
            {
                Ok(res) => res.damage.cloned(),
                Err(err) => {
                    tracing::warn!("Ошибка рендера: {err:?}");
                    None
                }
            };
            if let Some(path) = self.screenshot_request.take() {
                save_screenshot(renderer, &framebuffer, self.phys_w, self.phys_h, &path);
                // Readback switches to a surfaceless context. Restore the output target before
                // swapping; otherwise EGL can lose the window surface after a debug capture.
                use smithay::backend::renderer::{Renderer, Frame};
                if let Ok(frame) = renderer.render(&mut framebuffer, (self.phys_w, self.phys_h).into(), smithay::utils::Transform::Flipped180) {
                    let _ = frame.finish();
                }
            }
            result
        };
        match damage {
            Some(damage) if !damage.is_empty() => {
                let t_swap = Instant::now();
                if let Err(err) = backend.submit(Some(&damage)) {
                    tracing::warn!("Ошибка submit: {err:?}");
                    return None;
                }
                backend.window().request_redraw();
                Some(t_swap.elapsed())
            }
            _ => None,
        }
    }

    /// Direct DRM: compose (or scan out directly) and queue the frame for the next vblank
    fn render_drm(&mut self) -> Option<Duration> {
        let crate::backend::Backend::Drm(d)=&mut self.backend else{return None};
        let scale=self.scale_factor;let pointer=(self.pointer_location.x,self.pointer_location.y);
        let t=Instant::now();let mut submitted=false;
        for head in &mut d.heads {
            if head.pending {continue;}
            let Some(m)=self.monitors.iter().find(|m|m.info.name==head.name)else{continue};
            let info=&m.info;
            let elements=if self.lock.active {
                let mut e=cursor_elements(&mut d.renderer,&mut self.cursor,(pointer.0-info.rect.x as f64,pointer.1-info.rect.y as f64),scale);
                e.extend(self.lock.monitor_elements(&mut d.renderer,scale,info,self.shell.wallpaper_texture.as_ref().map(|t|(t,self.shell.wallpaper_texture_size))));
                output_elements(e,crate::monitors::Rect{x:0,y:0,..info.rect},scale)
            }else{
                let mut e=cursor_elements(&mut d.renderer,&mut self.cursor,pointer,scale);
                if info.primary {
                    e.extend(self.auth.elements(&mut d.renderer,scale,(self.shell.screen_w,self.shell.screen_h),(self.phys_w,self.phys_h)));
                    e.extend(self.audio_hud.elements(&mut d.renderer,scale,(self.shell.screen_w,self.shell.screen_h)));
                    e.extend(crate::screenshot::overlay(&mut d.renderer,self.screenshot_select.as_ref(),pointer,scale,(self.phys_w,self.phys_h)));
                }
                drag_overlay(&mut e,&mut d.renderer,&self.shell,pointer,scale);
                e.extend(build_scene(&mut d.renderer,&self.shell,&self.windows,scale,pointer,self.rounded_program.as_ref(),&self.animations,info.rect));
                output_elements(e,info.rect,scale)
            };
            match head.output.render_frame(&mut d.renderer,&elements,if self.lock.active {Color32F::new(0.0,0.0,0.0,1.0)}else{CLEAR_COLOR},smithay::backend::drm::compositor::FrameFlags::DEFAULT){
                Ok(r) if !r.is_empty=>match head.output.queue_frame(()){
                    Ok(())=>{head.pending=true;head.queued_protected=self.lock.active;submitted=true;},
                    Err(e)=>tracing::warn!("DRM {} queue: {e:?}",head.name),
                },
                Ok(_)=>{},Err(e)=>tracing::warn!("DRM {} render: {e:?}",head.name),
            }
        }
        // A failed lock frame must never leave the previous desktop on any connector.
        if self.lock.active && d.heads.iter().any(|h|!h.pending&&!h.protected){
            match d.clear_all(){Ok(())=>{submitted=false;self.lock.protected_frame=true;},Err(e)=>tracing::error!("Не удалось защитить все дисплеи: {e}")}
        }
        submitted.then(||t.elapsed())
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
        self.lock_presented();
        self.frame_pending = false;
    }

    pub fn send_frame_callbacks(&mut self) {
        let time = self.start_time.elapsed().as_millis() as u32;
        if self.lock.active {for (_,s) in &self.lock.extra_surfaces{send_frames_surface_tree(s.wl_surface(),time);}return;}
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
        if self.mission.is_some() {
            return None;
        }
        if self.lock.active {
            let r=self.monitor_at(px,py);return self.lock_focus().and_then(|surface|smithay::desktop::utils::under_from_surface_tree(&surface,(px,py).into(),(r.x,r.y),WindowSurfaceType::ALL).map(|(s,p)|(s,p.to_f64())));
        }
        if (self.shell.launchpad.is_some() && px>=0.0 && px<self.shell.screen_w as f64 && py>=0.0 && py<self.shell.screen_h as f64) || self.shell.switcher.as_ref().is_some_and(|s|s.visible) || self.icon_drag.as_ref().is_some_and(|d| d.active) {
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
