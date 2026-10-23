//! Rounded window corners on the GPU.
//!
//! Client surfaces are drawn with a custom texture shader that fades out pixels outside a
//! rounded rectangle (the window geometry), so square-cornered apps get macOS corners and the
//! bottom of server-decorated windows matches our rounded titlebar. No offscreen pass, no
//! extra memory: the shader only replaces the default one while these elements are drawn.

use smithay::{
    backend::renderer::{
        element::{
            surface::WaylandSurfaceRenderElement, utils::RescaleRenderElement, Element, Id, Kind,
            RenderElement, UnderlyingStorage,
        },
        gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform},
};

const SHADER: &str = r#"
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

// Texture size in pixels and the source rectangle sampled from it
uniform vec2 tex_size;
uniform vec4 src_rect;
// Destination of this element relative to the window geometry, in physical pixels
uniform vec4 dst_rect;
uniform vec2 geo_size;
uniform float radius;
// 1.0: round all four corners, 0.0: only the bottom ones (titlebar is above)
uniform float all_corners;

void main() {
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    vec2 t = (v_coords * tex_size - src_rect.xy) / src_rect.zw;
    vec2 p = dst_rect.xy + t * dst_rect.zw;
    bool in_corner_x = p.x < radius || p.x > geo_size.x - radius;
    bool in_corner_y = p.y < radius || p.y > geo_size.y - radius;
    bool allowed = all_corners > 0.5 || p.y > geo_size.y * 0.5;
    if (in_corner_x && in_corner_y && allowed) {
        vec2 c = clamp(p, vec2(radius), geo_size - vec2(radius));
        float d = distance(p, c);
        color *= clamp(radius - d + 0.5, 0.0, 1.0);
    }

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
"#;

pub fn compile(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    let uniforms = [
        UniformName::new("tex_size", UniformType::_2f),
        UniformName::new("src_rect", UniformType::_4f),
        UniformName::new("dst_rect", UniformType::_4f),
        UniformName::new("geo_size", UniformType::_2f),
        UniformName::new("radius", UniformType::_1f),
        UniformName::new("all_corners", UniformType::_1f),
    ];
    match renderer.compile_custom_texture_shader(SHADER, &uniforms) {
        Ok(p) => Some(p),
        Err(err) => {
            tracing::warn!("Шейдер скругления не скомпилирован: {err:?}");
            None
        }
    }
}

/// How a window's surfaces are clipped
#[derive(Debug, Clone, Copy)]
pub struct Clip {
    /// Window geometry in physical output coordinates
    pub geo: Rectangle<i32, Physical>,
    pub radius: f32,
    pub all_corners: bool,
}

pub struct RoundedSurfaceElement {
    inner: RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    program: GlesTexProgram,
    clip: Clip,
    /// Texture size of the surface buffer in pixels
    tex_size: Size<i32, Buffer>,
}

impl RoundedSurfaceElement {
    pub fn new(
        inner: RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
        program: GlesTexProgram,
        clip: Clip,
        tex_size: Size<i32, Buffer>,
    ) -> Self {
        Self {
            inner,
            program,
            clip,
            tex_size,
        }
    }

    /// Corner squares (output coordinates) that are not opaque any more
    fn corner_rects(&self) -> Vec<Rectangle<i32, Physical>> {
        let g = self.clip.geo;
        let r = self.clip.radius.ceil() as i32;
        let mut v = vec![
            Rectangle::new((g.loc.x, g.loc.y + g.size.h - r).into(), (r, r).into()),
            Rectangle::new((g.loc.x + g.size.w - r, g.loc.y + g.size.h - r).into(), (r, r).into()),
        ];
        if self.clip.all_corners {
            v.push(Rectangle::new(g.loc, (r, r).into()));
            v.push(Rectangle::new((g.loc.x + g.size.w - r, g.loc.y).into(), (r, r).into()));
        }
        v
    }
}

impl Element for RoundedSurfaceElement {
    fn id(&self) -> &Id {
        self.inner.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }
    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }
    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // Regions are relative to the element; subtract the transparent corners
        let loc = self.inner.geometry(scale).loc;
        let corners: Vec<_> = self
            .corner_rects()
            .into_iter()
            .map(|mut r| {
                r.loc -= loc;
                r
            })
            .collect();
        self.inner
            .opaque_regions(scale)
            .into_iter()
            .flat_map(|r| r.subtract_rects(corners.iter().copied()))
            .collect()
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for RoundedSurfaceElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        // Outer GPU transforms change dst. Work in the original element's local coordinates
        // and apply its final scale to the clip too (translation cancels out).
        let original = self.inner.geometry(Scale::from(1.0));
        let rel = original.loc - self.clip.geo.loc;
        let sx = dst.size.w as f32 / original.size.w.max(1) as f32;
        let sy = dst.size.h as f32 / original.size.h.max(1) as f32;
        frame.override_default_tex_program(
            self.program.clone(),
            vec![
                Uniform::new("tex_size", (self.tex_size.w as f32, self.tex_size.h as f32)),
                Uniform::new(
                    "src_rect",
                    (src.loc.x as f32, src.loc.y as f32, src.size.w as f32, src.size.h as f32),
                ),
                Uniform::new(
                    "dst_rect",
                    (rel.x as f32 * sx, rel.y as f32 * sy, dst.size.w as f32, dst.size.h as f32),
                ),
                Uniform::new(
                    "geo_size",
                    (self.clip.geo.size.w as f32 * sx, self.clip.geo.size.h as f32 * sy),
                ),
                Uniform::new("radius", self.clip.radius * sx.min(sy)),
                Uniform::new("all_corners", if self.clip.all_corners { 1.0f32 } else { 0.0 }),
            ],
        );
        let res = self.inner.draw(frame, src, dst, damage, opaque_regions);
        frame.clear_tex_program_override();
        res
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        // Direct scan-out would bypass the shader and show square corners
        None
    }
}
