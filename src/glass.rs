//! Live Liquid Glass on the GPU (macOS Tahoe optical frosted glass).
//!
//! Instead of sampling a static blurred copy of the wallpaper on the CPU, shell panels (the menu
//! bar, Dock, menus, Control Center, Spotlight, Notification Center, toast banners, Alt+Tab, the
//! App Ring, Screen Recorder HUD and the Drop Shelf) sit over `GlassElement`s that sample a live
//! downsampled backdrop blurred with a 2-level Dual-Kawase pyramid and bent at the capsule rim
//! with an analytical rounded-rectangle SDF bevel lens.
//!
//! Budget discipline (`docs/SPEC.md` §2):
//! - **0 work in idle & uncovered motion:** an `OutputDamageTracker` updates `scene_tex` (`1/4`
//!   output resolution) incrementally (`age = 1`). `valid_blur` records which regions of
//!   `blur_tex` already match `scene_tex`. World damage outside active glass panels (+ blur halo)
//!   leaves their `valid_blur` intact, so typing or animating 60 FPS in an uncovered window runs
//!   **zero** Kawase blur passes and **zero** FBO binds.
//! - **Sub-millisecond active blur:** when a window moves behind or beside a panel, Kawase runs
//!   only on the damaged intersection box at `1/4 → 1/8 → 1/16 → 1/8 → 1/4` resolution.
//! - **Zero `RssAnon`:** all four small pyramid textures live in GPU memory; fullscreen windows
//!   that hide the shell have no glass specs and keep direct scanout untouched.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{
                texture::{TextureBuffer, TextureRenderElement},
                Element, Id, Kind, RenderElement, UnderlyingStorage,
            },
            gles::{
                ffi, GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
                UniformName, UniformType,
            },
            utils::{CommitCounter, DamageSet, OpaqueRegions},
            Bind, Frame, Offscreen, Renderer,
        },
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Transform},
};

use crate::{animation::Visual, render::OutputElement, shell::R};

pub const KEY_TOPBAR: u64 = 1;
pub const KEY_DOCK: u64 = 2;
pub const KEY_DOCK_LABEL: u64 = 3;
pub const KEY_SWITCHER: u64 = 4;
pub const KEY_RING: u64 = 5;
pub const KEY_MISSION_TITLE: u64 = 6;
pub const KEY_SCREENCAST: u64 = 7;
pub const KEY_BANNER: u64 = 8;
pub const KEY_AUDIO_HUD: u64 = 9;
pub const KEY_DROP_SHELF: u64 = 10;
pub const KEY_TOAST_BASE: u64 = 100;
pub const KEY_OVERLAY_BASE: u64 = 200;

static LIVE_GLASS: AtomicBool = AtomicBool::new(false);

pub fn set_live_glass_enabled(on: bool) {
    LIVE_GLASS.store(on, Ordering::Relaxed);
}

/// True when GPU Liquid Glass shaders are active and "Reduce transparency" is off
#[inline]
pub fn live_glass_active() -> bool {
    LIVE_GLASS.load(Ordering::Relaxed) && !crate::theme::reduce_transparency()
}

/// Halo in `1/4`-resolution pixels (`14 * 4 = 56` physical px) around world damage that can
/// influence blurred pixels inside a nearby glass panel.
const KAWASE_HALO: i32 = 14;
/// Extra border in `1/4`-resolution pixels around a panel's physical box for bevel refraction.
const REFRACT_PAD: i32 = 4;

const DOWN_SHADER: &str = r#"
#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec2 halfpixel;
uniform vec4 uv_clamp;

vec4 tap(vec2 uv) {
    return texture2D(tex, clamp(uv, uv_clamp.xy, uv_clamp.zw));
}

void main() {
    vec2 uv = v_coords;
    vec4 sum = tap(uv) * 4.0;
    sum += tap(uv - halfpixel);
    sum += tap(uv + halfpixel);
    sum += tap(uv + vec2(halfpixel.x, -halfpixel.y));
    sum += tap(uv - vec2(halfpixel.x, -halfpixel.y));
    gl_FragColor = vec4((sum * 0.125).rgb, 1.0) * alpha;
}
"#;

const UP_SHADER: &str = r#"
#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec2 halfpixel;
uniform vec4 uv_clamp;

vec4 tap(vec2 uv) {
    return texture2D(tex, clamp(uv, uv_clamp.xy, uv_clamp.zw));
}

void main() {
    vec2 uv = v_coords;
    vec2 hp = halfpixel;
    vec4 sum = tap(uv + vec2(-hp.x * 2.0, 0.0));
    sum += tap(uv + vec2(-hp.x, hp.y)) * 2.0;
    sum += tap(uv + vec2(0.0, hp.y * 2.0));
    sum += tap(uv + vec2(hp.x, hp.y)) * 2.0;
    sum += tap(uv + vec2(hp.x * 2.0, 0.0));
    sum += tap(uv + vec2(hp.x, -hp.y)) * 2.0;
    sum += tap(uv + vec2(0.0, -hp.y * 2.0));
    sum += tap(uv + vec2(-hp.x, -hp.y)) * 2.0;
    gl_FragColor = vec4((sum * (1.0 / 12.0)).rgb, 1.0) * alpha;
}
"#;

const GLASS_SHADER: &str = r#"
#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec2 tex_size;
uniform vec4 src_rect;
// Final destination of this glass lens on the output, in physical pixels
uniform vec4 dst_rect;
uniform vec2 out_size;
uniform float radius;
// 0.0 .. 1.2: optical bevel refraction & specular rim strength
uniform float refract_strength;
uniform float scale_px;

vec2 clamp_uv(vec2 uv) {
    vec2 half_texel = vec2(0.5) / tex_size;
    return clamp(uv, half_texel, vec2(1.0) - half_texel);
}

void main() {
    vec2 t = (v_coords * tex_size - src_rect.xy) / max(src_rect.zw, vec2(0.0001));
    vec2 size = dst_rect.zw;
    vec2 p = t * size;

    float r = clamp(radius, 0.0, min(size.x, size.y) * 0.5);
    vec2 half_s = size * 0.5;
    vec2 rel = p - half_s;
    vec2 q = abs(rel) - (half_s - vec2(r));
    vec2 q_pos = max(q, vec2(0.0));
    float q_len = length(q_pos);
    float sdf = q_len + min(max(q.x, q.y), 0.0) - r;

    float cov = clamp(0.5 - sdf, 0.0, 1.0);
    if (cov <= 0.001) {
        gl_FragColor = vec4(0.0);
        return;
    }

    // Outward analytical normal of the rounded capsule
    vec2 grad;
    if (q_len > 0.0001) {
        grad = q_pos / q_len;
    } else if (q.x > q.y) {
        grad = vec2(1.0, 0.0);
    } else {
        grad = vec2(0.0, 1.0);
    }
    vec2 normal = grad * sign(rel);

    float bevel = min(14.0 * scale_px, min(size.x, size.y) * 0.38);
    float rim = 0.0;
    if (bevel > 0.5 && refract_strength > 0.001) {
        float x = clamp(1.0 - max(-sdf, 0.0) / bevel, 0.0, 1.0);
        rim = x * x * (3.0 - 2.0 * x);
    }

    // Curved glass rim pulls background inward along -normal with subtle prism dispersion
    float shift_px = rim * 8.5 * scale_px * refract_strength;
    vec2 screen_p = dst_rect.xy + p;
    vec2 base_uv = screen_p / max(out_size, vec2(1.0));
    vec2 shift_uv = (-normal * shift_px) / max(out_size, vec2(1.0));

    vec3 col;
    if (shift_px > 0.08) {
        float cr = texture2D(tex, clamp_uv(base_uv + shift_uv * 0.90)).r;
        vec4 cg = texture2D(tex, clamp_uv(base_uv + shift_uv));
        float cb = texture2D(tex, clamp_uv(base_uv + shift_uv * 1.10)).b;
        col = vec3(cr, cg.g, cb);
    } else {
        col = texture2D(tex, clamp_uv(base_uv)).rgb;
    }

    // macOS Tahoe vibrancy: keep colours vivid instead of washing out under translucent tint
    float luma = dot(col, vec3(0.2126, 0.7152, 0.0722));
    col = clamp(mix(vec3(luma), col, 1.18), 0.0, 1.0);

    // Subtle optical specular caustic right inside the curved glass lip
    if (refract_strength > 0.05 && r > 0.5) {
        float edge_band = clamp(1.0 - max(-sdf, 0.0) / max(2.6 * scale_px, 1.0), 0.0, 1.0);
        float light = max(dot(normal, vec2(-0.36, -0.93)), 0.0);
        float bounce = max(dot(normal, vec2(0.36, 0.93)), 0.0);
        col += vec3(edge_band * (0.075 * light + 0.028 * bounce) * refract_strength);
        col = clamp(col, 0.0, 1.0);
    }

    float out_a = cov * alpha;
    vec4 color = vec4(col * out_a, out_a);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.2, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
"#;

/// Logical description of one glass capsule to place in the scene
#[derive(Debug, Clone, Copy)]
pub struct GlassSpec {
    /// Stable slot identifier so Smithay's damage tracker tracks each capsule cleanly
    pub key: u64,
    /// Logical rectangle in primary/global coordinates
    pub rect: R,
    /// Corner radius in logical points (`0.0` for flat bars like the top menu bar)
    pub radius: f32,
    /// Bevel lens strength (`0.0..=1.2`)
    pub strength: f32,
    /// Spring transform matching the CPU overlay/panel above it
    pub visual: Visual,
}

/// Pure-CPU snapshot of the world behind glass panels so static frames and uncovered window
/// motion skip `GlesRenderer` / FBO binds completely.
#[derive(Clone, PartialEq, Eq)]
pub struct WorldSnap {
    pub globals_hash: u64,
    /// Front-to-back `(window_id, l0_bounds_with_shadow, window_state_hash)`
    pub windows: Vec<(u64, Rectangle<i32, Physical>, u64)>,
}

pub fn head_l0_size(view: crate::monitors::Rect) -> (i32, i32) {
    (
        ((view.w + 3) / 4).clamp(16, 640),
        ((view.h + 3) / 4).clamp(16, 400),
    )
}

#[derive(Default)]
struct SlotTrack {
    id: Option<Id>,
    commit: CommitCounter,
}

struct GlassHead {
    out_size: (i32, i32),
    l0_size: (i32, i32),
    scene_tex: GlesTexture,
    down1_tex: GlesTexture,
    down2_tex: GlesTexture,
    blur_tex: GlesTexture,
    blur_buffer: TextureBuffer<GlesTexture>,
    tracker: OutputDamageTracker,
    fresh: bool,
    scene_dirty: bool,
    last_snap: Option<WorldSnap>,
    /// Regions of `blur_tex` (in `l0_size` coordinates) whose Dual-Kawase blur is up to date
    valid_blur: Vec<Rectangle<i32, Physical>>,
    slots: HashMap<u64, SlotTrack>,
}

pub struct GlassState {
    down_program: GlesTexProgram,
    up_program: GlesTexProgram,
    glass_program: GlesTexProgram,
    heads: HashMap<String, GlassHead>,
    /// Diagnostic counter incremented per Kawase blur batch (for `FEATHER_DEBUG_GLASS` / profile)
    pub blur_passes: u64,
}

impl GlassState {
    pub fn compile(renderer: &mut GlesRenderer) -> Option<Self> {
        if std::env::var("FEATHER_GLASS").as_deref() == Ok("0") {
            set_live_glass_enabled(false);
            return None;
        }
        let pass_uniforms = [
            UniformName::new("halfpixel", UniformType::_2f),
            UniformName::new("uv_clamp", UniformType::_4f),
        ];
        let glass_uniforms = [
            UniformName::new("tex_size", UniformType::_2f),
            UniformName::new("src_rect", UniformType::_4f),
            UniformName::new("dst_rect", UniformType::_4f),
            UniformName::new("out_size", UniformType::_2f),
            UniformName::new("radius", UniformType::_1f),
            UniformName::new("refract_strength", UniformType::_1f),
            UniformName::new("scale_px", UniformType::_1f),
        ];
        let down_program = match renderer.compile_custom_texture_shader(DOWN_SHADER, &pass_uniforms) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!("Шейдер Kawase down не скомпилирован: {err:?}");
                set_live_glass_enabled(false);
                return None;
            }
        };
        let up_program = match renderer.compile_custom_texture_shader(UP_SHADER, &pass_uniforms) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!("Шейдер Kawase up не скомпилирован: {err:?}");
                set_live_glass_enabled(false);
                return None;
            }
        };
        let glass_program = match renderer.compile_custom_texture_shader(GLASS_SHADER, &glass_uniforms) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!("Шейдер Живого стекла не скомпилирован: {err:?}");
                set_live_glass_enabled(false);
                return None;
            }
        };
        set_live_glass_enabled(true);
        Some(Self {
            down_program,
            up_program,
            glass_program,
            heads: HashMap::new(),
            blur_passes: 0,
        })
    }

    pub fn retain_heads(&mut self, keep: impl Fn(&str) -> bool) {
        self.heads.retain(|name, _| keep(name));
    }

    /// Updates `scene_tex` and runs Dual-Kawase blur strictly for regions needed by `specs` that
    /// are not yet valid in `blur_tex`. Pure-CPU checks run first so `get_renderer` and
    /// `build_world` are never invoked when all active glass panels already have valid blur.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare<'a>(
        &mut self,
        head_name: &str,
        out_size: (i32, i32),
        view: crate::monitors::Rect,
        scale: f64,
        specs: &[GlassSpec],
        snap: WorldSnap,
        get_renderer: impl FnOnce() -> &'a mut GlesRenderer,
        build_world: impl FnOnce(&mut GlesRenderer, f64) -> Vec<OutputElement>,
    ) {
        if !live_glass_active() || specs.is_empty() || out_size.0 <= 0 || out_size.1 <= 0 {
            return;
        }
        let l0_size = head_l0_size(view);
        let (l0, h0) = l0_size;
        let l1_size = ((l0 / 2).max(8), (h0 / 2).max(8));
        let l2_size = ((l1_size.0 / 2).max(4), (l1_size.1 / 2).max(4));
        let bounds0 = Rectangle::from_size(l0_size.into());

        let mut get_renderer = Some(get_renderer);
        let recreate = self
            .heads
            .get(head_name)
            .is_none_or(|h| h.out_size != out_size || h.l0_size != l0_size);
        let mut renderer_opt: Option<&mut GlesRenderer> = None;
        if recreate {
            let r = (get_renderer.take().unwrap())();
            let Some(new_head) = create_head(r, out_size, l0_size, l1_size, l2_size) else {
                return;
            };
            self.heads.insert(head_name.to_string(), new_head);
            renderer_opt = Some(r);
        }

        let Some(head) = self.heads.get_mut(head_name) else { return };
        match head.last_snap.as_ref() {
            None => {
                head.valid_blur.clear();
                head.scene_dirty = true;
            }
            Some(prev) if prev.globals_hash != snap.globals_hash => {
                head.valid_blur.clear();
                head.scene_dirty = true;
            }
            Some(prev) if prev.windows != snap.windows => {
                let mut dirty_l0: Vec<Rectangle<i32, Physical>> = Vec::new();
                for (pid, prect, phash) in &prev.windows {
                    match snap.windows.iter().find(|(cid, _, _)| cid == pid) {
                        None => dirty_l0.push(*prect),
                        Some((_, crect, chash)) if crect != prect || chash != phash => {
                            dirty_l0.push(*prect);
                            dirty_l0.push(*crect);
                        }
                        _ => {}
                    }
                }
                for (cid, crect, _) in &snap.windows {
                    if !prev.windows.iter().any(|(pid, _, _)| pid == cid) {
                        dirty_l0.push(*crect);
                    }
                }
                // Pure stacking-order swap between two overlapping windows
                if dirty_l0.is_empty() {
                    for (i, (cid, crect, _)) in snap.windows.iter().enumerate() {
                        if prev.windows.get(i).map(|(pid, _, _)| pid) != Some(cid) {
                            dirty_l0.push(*crect);
                        }
                    }
                }
                head.scene_dirty = true;
                invalidate_valid_blur(&mut head.valid_blur, &dirty_l0, bounds0, KAWASE_HALO);
            }
            Some(_) => {}
        }
        head.last_snap = Some(snap);

        // Project every visible glass panel's animated output rect into `l0_size` coords.
        // When a panel is actively springing (`!visual.identity()`), cover its full resting +
        // overshoot envelope on frame 1 so intermediate spring frames run 0 Kawase passes.
        let sx0 = l0_size.0 as f64 / out_size.0 as f64;
        let sy0 = l0_size.1 as f64 / out_size.1 as f64;
        let mut panel_l0: Vec<(u64, Rectangle<i32, Physical>)> = Vec::with_capacity(specs.len());
        let mut needed: Vec<Rectangle<i32, Physical>> = Vec::with_capacity(specs.len());
        for spec in specs {
            if spec.visual.alpha <= 0.001 || spec.rect.w <= 0.0 || spec.rect.h <= 0.0 {
                continue;
            }
            let out_r = spec_output_rect(spec, view, scale);
            if let Some(r0) = expand_clamped(
                scale_rect_ceil(out_r, sx0, sy0),
                REFRACT_PAD,
                bounds0,
            ) {
                panel_l0.push((spec.key, r0));
                let need_out = if spec.visual.identity() {
                    out_r
                } else {
                    out_r.merge(spec_spring_envelope_rect(spec, view, scale))
                };
                if let Some(rn) = expand_clamped(
                    scale_rect_ceil(need_out, sx0, sy0),
                    REFRACT_PAD,
                    bounds0,
                ) {
                    needed.push(rn);
                } else {
                    needed.push(r0);
                }
            }
        }

        let mut missing = missing_blur_regions(&needed, &head.valid_blur);
        if missing.is_empty() {
            return;
        }

        let renderer = match renderer_opt {
            Some(r) => r,
            None => (get_renderer.take().unwrap())(),
        };
        if head.fresh || head.scene_dirty {
            let blur_scale_x = l0_size.0 as f64 / (view.w.max(1) as f64);
            let blur_scale_y = l0_size.1 as f64 / (view.h.max(1) as f64);
            let blur_scale = (blur_scale_x + blur_scale_y) * 0.5;
            let world_elements = build_world(renderer, blur_scale);
            let age = if head.fresh { 0 } else { 1 };
            let Ok(mut fb) = renderer.bind(&mut head.scene_tex) else { return };
            match head.tracker.render_output(
                renderer,
                &mut fb,
                age,
                &world_elements,
                crate::render::CLEAR_COLOR,
            ) {
                Ok(res) => {
                    head.fresh = false;
                    head.scene_dirty = false;
                    if let Some(dmg) = res.damage {
                        invalidate_valid_blur(&mut head.valid_blur, dmg, bounds0, KAWASE_HALO);
                        missing = missing_blur_regions(&needed, &head.valid_blur);
                    }
                }
                Err(err) => {
                    tracing::warn!("Ошибка рендера фона Живого стекла: {err:?}");
                    return;
                }
            }
        }
        if missing.is_empty() {
            return;
        }

        // Coalesce overlapping/adjacent missing boxes and execute all regions in a single 4-pass pyramid
        let batches = coalesce_rects(missing, 8);
        if run_kawase_batches(
            renderer,
            &self.down_program,
            &self.up_program,
            &head.scene_tex,
            &mut head.down1_tex,
            &mut head.down2_tex,
            &mut head.blur_tex,
            l0_size,
            l1_size,
            l2_size,
            &batches,
        )
        .is_ok()
        {
            for box0 in &batches {
                record_valid_blur(&mut head.valid_blur, *box0);
            }
            self.blur_passes += batches.len() as u64;
        }

        // Bump commit counter ONLY for panels whose backdrop blur actually changed
        for (key, r0) in panel_l0 {
            if batches.iter().any(|b| b.overlaps(r0)) {
                let slot = head.slots.entry(key).or_insert_with(|| SlotTrack {
                    id: Some(Id::new()),
                    commit: CommitCounter::default(),
                });
                if slot.id.is_none() {
                    slot.id = Some(Id::new());
                }
                slot.commit.increment();
            }
        }
    }

    /// Creates a `GlassElement` in global physical scene coordinates (before `output_elements`
    /// shifts by `-view.loc * scale`). Apply `spec.visual.element(..., scale)` to animate it.
    pub fn element(&mut self, head_name: &str, spec: &GlassSpec, scale: f64) -> Option<GlassElement> {
        if !live_glass_active() || spec.visual.alpha <= 0.001 || spec.rect.w <= 0.0 || spec.rect.h <= 0.0 {
            return None;
        }
        let head = self.heads.get_mut(head_name)?;
        let slot = head.slots.entry(spec.key).or_insert_with(|| SlotTrack {
            id: Some(Id::new()),
            commit: CommitCounter::default(),
        });
        let id = slot.id.get_or_insert_with(Id::new).clone();
        let commit = slot.commit;

        let gx = (spec.rect.x as f64 * scale).round();
        let gy = (spec.rect.y as f64 * scale).round();
        let gw = ((spec.rect.w as f64 * scale).round() as i32).max(1);
        let gh = ((spec.rect.h as f64 * scale).round() as i32).max(1);
        let src = Rectangle::from_size((head.l0_size.0 as f64, head.l0_size.1 as f64).into());
        let inner = TextureRenderElement::from_texture_buffer(
            (gx, gy),
            &head.blur_buffer,
            Some(spec.visual.alpha),
            Some(src),
            Some((gw, gh).into()),
            Kind::Unspecified,
        );
        Some(GlassElement {
            inner,
            id,
            commit,
            program: self.glass_program.clone(),
            l0_size: head.l0_size,
            out_size: head.out_size,
            radius_phys: spec.radius * scale as f32,
            strength: spec.strength,
            scale_px: scale as f32,
        })
    }
}

fn create_clamped_texture(
    renderer: &mut GlesRenderer,
    size: (i32, i32),
) -> Option<GlesTexture> {
    let tex: GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, size.into())
        .ok()?;
    let id = tex.tex_id();
    let _ = renderer.with_context(|gl| unsafe {
        gl.BindTexture(ffi::TEXTURE_2D, id);
        gl.TexParameteri(
            ffi::TEXTURE_2D,
            ffi::TEXTURE_WRAP_S,
            ffi::CLAMP_TO_EDGE as i32,
        );
        gl.TexParameteri(
            ffi::TEXTURE_2D,
            ffi::TEXTURE_WRAP_T,
            ffi::CLAMP_TO_EDGE as i32,
        );
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
    });
    Some(tex)
}

fn create_head(
    renderer: &mut GlesRenderer,
    out_size: (i32, i32),
    l0_size: (i32, i32),
    l1_size: (i32, i32),
    l2_size: (i32, i32),
) -> Option<GlassHead> {
    let scene_tex = create_clamped_texture(renderer, l0_size)?;
    let down1_tex = create_clamped_texture(renderer, l1_size)?;
    let down2_tex = create_clamped_texture(renderer, l2_size)?;
    let blur_tex = create_clamped_texture(renderer, l0_size)?;
    let blur_buffer = TextureBuffer::from_texture(renderer, blur_tex.clone(), 1, Transform::Normal, None);
    let tracker = OutputDamageTracker::new(l0_size, 1.0, Transform::Normal);
    Some(GlassHead {
        out_size,
        l0_size,
        scene_tex,
        down1_tex,
        down2_tex,
        blur_tex,
        blur_buffer,
        tracker,
        fresh: true,
        scene_dirty: true,
        last_snap: None,
        valid_blur: Vec::new(),
        slots: HashMap::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn run_kawase_batches(
    renderer: &mut GlesRenderer,
    down_program: &GlesTexProgram,
    up_program: &GlesTexProgram,
    scene_tex: &GlesTexture,
    down1_tex: &mut GlesTexture,
    down2_tex: &mut GlesTexture,
    blur_tex: &mut GlesTexture,
    l0: (i32, i32),
    l1: (i32, i32),
    l2: (i32, i32),
    boxes0: &[Rectangle<i32, Physical>],
) -> Result<(), GlesError> {
    let b1 = Rectangle::from_size(l1.into());
    let b2 = Rectangle::from_size(l2.into());
    let sx01 = l1.0 as f64 / l0.0 as f64;
    let sy01 = l1.1 as f64 / l0.1 as f64;
    let sx12 = l2.0 as f64 / l1.0 as f64;
    let sy12 = l2.1 as f64 / l1.1 as f64;

    let mut m1_down = Vec::with_capacity(boxes0.len());
    let mut m2_down = Vec::with_capacity(boxes0.len());
    let mut m1_up = Vec::with_capacity(boxes0.len());
    for &box0 in boxes0 {
        if let Some(d1) = expand_clamped(scale_rect_ceil(box0, sx01, sy01), 6, b1) {
            m1_down.push(d1);
            if let Some(d2) = expand_clamped(scale_rect_ceil(d1, sx12, sy12), 3, b2) {
                m2_down.push(d2);
            }
        }
        if let Some(u1) = expand_clamped(scale_rect_ceil(box0, sx01, sy01), 3, b1) {
            m1_up.push(u1);
        }
    }
    if m1_down.is_empty() || m2_down.is_empty() || m1_up.is_empty() {
        return Ok(());
    }

    // Pass 1: scene_tex (l0) -> down1_tex (l1)
    kawase_pass(renderer, scene_tex, down1_tex, l0, l1, &m1_down, down_program, 1.15)?;
    // Pass 2: down1_tex (l1) -> down2_tex (l2)
    kawase_pass(renderer, down1_tex, down2_tex, l1, l2, &m2_down, down_program, 1.15)?;
    // Pass 3: down2_tex (l2) -> down1_tex (l1)
    kawase_pass(renderer, down2_tex, down1_tex, l2, l1, &m1_up, up_program, 1.15)?;
    // Pass 4: down1_tex (l1) -> blur_tex (l0)
    kawase_pass(renderer, down1_tex, blur_tex, l1, l0, boxes0, up_program, 1.15)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn kawase_pass(
    renderer: &mut GlesRenderer,
    src_tex: &GlesTexture,
    dst_tex: &mut GlesTexture,
    src_size: (i32, i32),
    dst_size: (i32, i32),
    dirty_dst: &[Rectangle<i32, Physical>],
    program: &GlesTexProgram,
    spread: f32,
) -> Result<(), GlesError> {
    let mut fb = renderer.bind(dst_tex)?;
    let mut frame = renderer.render(&mut fb, dst_size.into(), Transform::Normal)?;
    let hp_x = (0.5 * spread) / src_size.0.max(1) as f32;
    let hp_y = (0.5 * spread) / src_size.1.max(1) as f32;
    let eps_x = 0.5 / src_size.0.max(1) as f32;
    let eps_y = 0.5 / src_size.1.max(1) as f32;
    let uniforms = [
        Uniform::new("halfpixel", (hp_x, hp_y)),
        Uniform::new("uv_clamp", (eps_x, eps_y, 1.0 - eps_x, 1.0 - eps_y)),
    ];
    let src_rect = Rectangle::<f64, Buffer>::from_size((src_size.0 as f64, src_size.1 as f64).into());
    let dst_rect = Rectangle::<i32, Physical>::from_size(dst_size.into());
    let opaque = [dst_rect];
    frame.render_texture_from_to(
        src_tex,
        src_rect,
        dst_rect,
        dirty_dst,
        &opaque,
        Transform::Normal,
        1.0,
        Some(program),
        &uniforms,
    )?;
    let _ = frame.finish()?;
    Ok(())
}

/// Resting + spring overshoot envelope of `spec` in output-local physical pixels
fn spec_spring_envelope_rect(spec: &GlassSpec, view: crate::monitors::Rect, scale: f64) -> Rectangle<i32, Physical> {
    let pad = 6.0;
    let s = 1.04;
    let cx = spec.rect.x as f64 + spec.rect.w as f64 * 0.5;
    let cy = spec.rect.y as f64 + spec.rect.h as f64 * 0.5;
    let x0 = cx + (spec.rect.x as f64 - pad - cx) * s - view.x as f64;
    let y0 = cy + (spec.rect.y as f64 - pad - cy) * s - view.y as f64;
    let w = ((spec.rect.w as f64 + pad * 2.0) * s).max(1.0);
    let h = ((spec.rect.h as f64 + pad * 2.0) * s).max(1.0);
    let px = (x0 * scale).floor() as i32;
    let py = (y0 * scale).floor() as i32;
    let pw = (((x0 + w) * scale).ceil() as i32 - px).max(1);
    let ph = (((y0 + h) * scale).ceil() as i32 - py).max(1);
    Rectangle::new((px, py).into(), (pw, ph).into())
}

/// Output-local physical rectangle of `spec` after applying its `Visual` scale, pivot and offset
fn spec_output_rect(spec: &GlassSpec, view: crate::monitors::Rect, scale: f64) -> Rectangle<i32, Physical> {
    let v = spec.visual;
    let s = v.scale;
    let cx = v.center.0;
    let cy = v.center.1;
    let x0 = cx + (spec.rect.x as f64 - cx) * s + v.offset.0 - view.x as f64;
    let y0 = cy + (spec.rect.y as f64 - cy) * s + v.offset.1 - view.y as f64;
    let w = (spec.rect.w as f64 * s).max(1.0);
    let h = (spec.rect.h as f64 * s).max(1.0);
    let px = (x0 * scale).floor() as i32;
    let py = (y0 * scale).floor() as i32;
    let pw = (((x0 + w) * scale).ceil() as i32 - px).max(1);
    let ph = (((y0 + h) * scale).ceil() as i32 - py).max(1);
    Rectangle::new((px, py).into(), (pw, ph).into())
}

fn scale_rect_ceil(r: Rectangle<i32, Physical>, sx: f64, sy: f64) -> Rectangle<i32, Physical> {
    let x0 = (r.loc.x as f64 * sx).floor() as i32;
    let y0 = (r.loc.y as f64 * sy).floor() as i32;
    let x1 = ((r.loc.x + r.size.w) as f64 * sx).ceil() as i32;
    let y1 = ((r.loc.y + r.size.h) as f64 * sy).ceil() as i32;
    Rectangle::new((x0, y0).into(), ((x1 - x0).max(1), (y1 - y0).max(1)).into())
}

fn expand_clamped(
    r: Rectangle<i32, Physical>,
    pad: i32,
    bounds: Rectangle<i32, Physical>,
) -> Option<Rectangle<i32, Physical>> {
    let x0 = (r.loc.x - pad).max(bounds.loc.x);
    let y0 = (r.loc.y - pad).max(bounds.loc.y);
    let x1 = (r.loc.x + r.size.w + pad).min(bounds.loc.x + bounds.size.w);
    let y1 = (r.loc.y + r.size.h + pad).min(bounds.loc.y + bounds.size.h);
    (x1 > x0 && y1 > y0).then(|| Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into()))
}

fn invalidate_valid_blur(
    valid: &mut Vec<Rectangle<i32, Physical>>,
    world_damage: &[Rectangle<i32, Physical>],
    bounds: Rectangle<i32, Physical>,
    halo: i32,
) {
    if valid.is_empty() || world_damage.is_empty() {
        return;
    }
    let holes: Vec<_> = world_damage
        .iter()
        .filter_map(|d| expand_clamped(*d, halo, bounds))
        .collect();
    if holes.is_empty() {
        return;
    }
    let taken = std::mem::take(valid);
    *valid = Rectangle::subtract_rects_many_in_place(taken, holes);
}

fn missing_blur_regions(
    needed: &[Rectangle<i32, Physical>],
    valid: &[Rectangle<i32, Physical>],
) -> Vec<Rectangle<i32, Physical>> {
    if needed.is_empty() {
        return Vec::new();
    }
    Rectangle::subtract_rects_many_in_place(needed.to_vec(), valid.iter().copied())
}

fn record_valid_blur(valid: &mut Vec<Rectangle<i32, Physical>>, added: Rectangle<i32, Physical>) {
    let cleaned = Rectangle::subtract_rects_many_in_place(std::mem::take(valid), [added]);
    *valid = cleaned;
    valid.push(added);
    // Bound fragmentation across many small animations
    if valid.len() > 24 {
        valid.drain(0..valid.len() - 16);
    }
}

fn coalesce_rects(mut rects: Vec<Rectangle<i32, Physical>>, gap: i32) -> Vec<Rectangle<i32, Physical>> {
    let mut out: Vec<Rectangle<i32, Physical>> = Vec::with_capacity(rects.len().min(4));
    while let Some(mut cur) = rects.pop() {
        let mut merged = true;
        while merged {
            merged = false;
            let mut i = 0;
            while i < rects.len() {
                let r = rects[i];
                let expanded = Rectangle::new(
                    (cur.loc.x - gap, cur.loc.y - gap).into(),
                    (cur.size.w + gap * 2, cur.size.h + gap * 2).into(),
                );
                if expanded.overlaps(r) {
                    cur = cur.merge(r);
                    rects.swap_remove(i);
                    merged = true;
                } else {
                    i += 1;
                }
            }
        }
        out.push(cur);
    }
    if out.len() > 4 {
        if let Some(all) = out.into_iter().reduce(|a, b| a.merge(b)) {
            return vec![all];
        }
        return Vec::new();
    }
    out
}

pub struct GlassElement {
    inner: TextureRenderElement<GlesTexture>,
    id: Id,
    commit: CommitCounter,
    program: GlesTexProgram,
    l0_size: (i32, i32),
    out_size: (i32, i32),
    radius_phys: f32,
    strength: f32,
    scale_px: f32,
}

impl Element for GlassElement {
    fn id(&self) -> &Id {
        &self.id
    }
    fn current_commit(&self) -> CommitCounter {
        self.commit
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }
    fn transform(&self) -> Transform {
        Transform::Normal
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }
    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }
    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        if commit != Some(self.commit) {
            DamageSet::from_slice(&[Rectangle::from_size(self.geometry(scale).size)])
        } else {
            DamageSet::default()
        }
    }
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // Rounded corners and spring fade-ins blend with the scene; the center is opaque when
        // alpha == 1, which lets the damage tracker skip redrawing occluded window interiors!
        if self.inner.alpha() >= 0.999 {
            let g = self.inner.geometry(Scale::from(1.0));
            let r = (self.radius_phys.ceil() as i32).max(0);
            let inner_w = g.size.w - r * 2;
            let inner_h = g.size.h - r * 2;
            if r == 0 && g.size.w > 0 && g.size.h > 0 {
                return OpaqueRegions::from_slice(&[Rectangle::from_size(g.size)]);
            } else if inner_w > 0 && g.size.h > 0 {
                let mut regs = vec![Rectangle::new((r, 0).into(), (inner_w, g.size.h).into())];
                if inner_h > 0 && r > 0 {
                    regs.push(Rectangle::new((0, r).into(), (r, inner_h).into()));
                    regs.push(Rectangle::new((g.size.w - r, r).into(), (r, inner_h).into()));
                }
                return regs.into_iter().collect();
            }
        }
        OpaqueRegions::default()
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for GlassElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let orig = self.inner.geometry(Scale::from(1.0));
        let sx = dst.size.w as f32 / orig.size.w.max(1) as f32;
        let sy = dst.size.h as f32 / orig.size.h.max(1) as f32;
        let scale_k = sx.min(sy);
        let uniforms = [
            Uniform::new("tex_size", (self.l0_size.0 as f32, self.l0_size.1 as f32)),
            Uniform::new(
                "src_rect",
                (
                    src.loc.x as f32,
                    src.loc.y as f32,
                    src.size.w as f32,
                    src.size.h as f32,
                ),
            ),
            Uniform::new(
                "dst_rect",
                (
                    dst.loc.x as f32,
                    dst.loc.y as f32,
                    dst.size.w as f32,
                    dst.size.h as f32,
                ),
            ),
            Uniform::new("out_size", (self.out_size.0 as f32, self.out_size.1 as f32)),
            Uniform::new("radius", self.radius_phys * scale_k),
            Uniform::new("refract_strength", self.strength),
            Uniform::new("scale_px", self.scale_px * scale_k),
        ];
        frame.override_default_tex_program(self.program.clone(), uniforms.to_vec());
        let res = RenderElement::<GlesRenderer>::draw(&self.inner, frame, src, dst, damage, opaque_regions);
        frame.clear_tex_program_override();
        res
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn uncovered_world_damage_keeps_topbar_and_dock_blur_valid() {
        let bounds = rect(0, 0, 360, 225);
        let topbar = rect(0, 0, 360, 10);
        let dock = rect(90, 200, 180, 25);
        let mut valid = Vec::new();
        record_valid_blur(&mut valid, topbar);
        record_valid_blur(&mut valid, dock);
        assert!(missing_blur_regions(&[topbar, dock], &valid).is_empty());

        // Typing / 60 FPS animation in the center of the screen (away from KAWASE_HALO of bars)
        invalidate_valid_blur(&mut valid, &[rect(60, 50, 220, 110)], bounds, KAWASE_HALO);
        assert!(
            missing_blur_regions(&[topbar, dock], &valid).is_empty(),
            "center damage must not trigger any Kawase blur passes for topbar or dock"
        );

        // Window moves into the halo right above the Dock: only Dock needs re-blurring, not Topbar
        invalidate_valid_blur(&mut valid, &[rect(120, 190, 80, 12)], bounds, KAWASE_HALO);
        let miss = missing_blur_regions(&[topbar, dock], &valid);
        assert!(!miss.is_empty(), "halo overlap must invalidate Dock blur");
        assert!(
            miss.iter().all(|m| !m.overlaps(topbar)),
            "Topbar must stay valid when damage is near the Dock"
        );
    }

    #[test]
    fn opening_panel_over_static_desktop_blurs_only_new_panel_once() {
        let topbar = rect(0, 0, 360, 10);
        let spotlight = rect(110, 70, 140, 60);
        let mut valid = Vec::new();
        record_valid_blur(&mut valid, topbar);

        let miss1 = missing_blur_regions(&[topbar, spotlight], &valid);
        assert_eq!(miss1, vec![spotlight]);
        for m in coalesce_rects(miss1, 8) {
            record_valid_blur(&mut valid, m);
        }
        assert!(
            missing_blur_regions(&[topbar, spotlight], &valid).is_empty(),
            "subsequent spring frames over a static desktop require zero Kawase passes"
        );
    }
}
