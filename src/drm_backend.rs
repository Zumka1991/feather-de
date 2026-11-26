//! Direct DRM/KMS backend: one scanout and damage history per connected connector.
//! The primary GPU composes every scene; hotplug is driven by udev, never polling.
//! Secondary GPUs (hybrid AMD/NVIDIA laptops, docks) run their own EGL/GLES renderer and
//! native scanout swapchain so each driver uses its own formats/modifiers and never submits
//! foreign VRAM buffers into another GPU's command stream. Frames cross from primary to
//! secondary via verified GPU DMA-BUF sampling (with native fence sync) or damage-rect CPU copy.
use crate::{render::OutputElement, FeatherCompositor};
use smithay::{
    backend::{
        allocator::{
            dmabuf::{AsDmabuf, Dmabuf},
            format::FormatSet,
            gbm::{GbmAllocator, GbmBuffer, GbmBufferFlags, GbmDevice},
            Allocator, Buffer as AllocBuffer, Fourcc, Modifier,
        },
        drm::{
            compositor::{FrameFlags, PrimaryPlaneElement},
            exporter::{gbm::GbmFramebufferExporter, ExportBuffer, ExportFramebuffer},
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
        },
        egl::{context::ContextPriority, EGLContext, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            damage::OutputDamageTracker,
            element::{texture::TextureRenderElement, Id, Kind, RenderElementStates},
            gles::{GlesRenderer, GlesTexture},
            sync::SyncPoint,
            utils::DamageBag,
            Bind, Color32F, ExportMem, Frame, ImportDma, ImportMem, Offscreen, Renderer,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{all_gpus, primary_gpu, UdevBackend, UdevEvent},
    },
    output::OutputModeSource,
    reexports::{
        calloop::{LoopHandle, RegistrationToken},
        drm::{
            control::{connector, crtc, Device as ControlDevice, ModeTypeFlags},
            Device as DrmCoreDevice,
        },
        input::Libinput,
        rustix::fs::OFlags,
    },
    utils::{Buffer as BufferCoord, DeviceFd, Physical, Rectangle, Size, Transform},
};
use std::path::PathBuf;
/// A queued frame carries the wp_presentation feedback of what it shows
pub type FrameData = Option<smithay::desktop::utils::OutputPresentationFeedback>;
pub type GbmDrmOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, FrameData, DrmDeviceFd>;
pub type GbmDrmOutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    FrameData,
    DrmDeviceFd,
>;
const COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Xrgb8888];
const MAX_HEADS: usize = 8;
const MAX_CPU_COPY_RECTS: usize = 4;
const OPEN_FLAGS: OFlags = OFlags::RDWR.union(OFlags::CLOEXEC).union(OFlags::NOCTTY).union(OFlags::NONBLOCK);

/// User override for transferring frames from primary GPU to secondary GPUs (`FEATHER_MULTIGPU_COPY`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MultiGpuCopyMode {
    Auto,
    Dma,
    Cpu,
}

fn multigpu_copy_mode() -> MultiGpuCopyMode {
    match std::env::var("FEATHER_MULTIGPU_COPY")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("cpu" | "CPU") => MultiGpuCopyMode::Cpu,
        Some("dma" | "DMA") => MultiGpuCopyMode::Dma,
        _ => MultiGpuCopyMode::Auto,
    }
}

/// Strategy for moving a composed frame from the primary GPU to a secondary GPU's renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyStrategy {
    /// Primary allocates & renders into its own shared Dmabuf; secondary imports it as a texture
    /// (2D or EXTERNAL_OES) and blits into its native scanout swapchain with GPU fence sync.
    DmaCopy {
        fourcc: Fourcc,
        modifiers: Vec<Modifier>,
    },
    /// Primary renders into an offscreen texture and copies only damaged rectangles via PBO
    /// into a persistent texture on the secondary GPU.
    CpuCopy,
}

enum BridgeBacking {
    Dma {
        _bo: GbmBuffer,
        dmabuf: Dmabuf,
        last_read_sync: Option<SyncPoint>,
    },
    Cpu {
        offscreen: GlesTexture,
        sec_texture: GlesTexture,
    },
}

/// Per-head offscreen bridge for an output driven by a secondary GPU with its own GLES renderer.
pub struct SecondaryBridge {
    size: (i32, i32),
    backing: BridgeBacking,
    damage_tracker: OutputDamageTracker,
    damage_bag: DamageBag<i32, BufferCoord>,
    element_id: Id,
    initialized: bool,
}

impl SecondaryBridge {
    fn new_dma(
        size: (i32, i32),
        primary_gbm: &GbmDevice<DrmDeviceFd>,
        primary_renderer: &mut GlesRenderer,
        sec_renderer: &mut GlesRenderer,
        fourcc: Fourcc,
        modifiers: &[Modifier],
    ) -> Result<Self, String> {
        let mut alloc = GbmAllocator::new(primary_gbm.clone(), GbmBufferFlags::RENDERING);
        let bo = alloc
            .create_buffer(size.0 as u32, size.1 as u32, fourcc, modifiers)
            .map_err(|e| format!("allocate shared dmabuf {}x{}: {e}", size.0, size.1))?;
        let mut dmabuf = bo.export().map_err(|e| format!("export shared dmabuf: {e}"))?;
        if dmabuf.format().modifier == Modifier::Invalid {
            return Err("shared dmabuf fell back to implicit modifier".into());
        }
        // Validate bind on primary and import on secondary up front so we fall back cleanly.
        {
            let _fb = primary_renderer
                .bind(&mut dmabuf)
                .map_err(|e| format!("primary bind shared dmabuf: {e}"))?;
        }
        let buf_size = Size::<i32, BufferCoord>::from(size);
        let _tex = sec_renderer
            .import_dmabuf(&dmabuf, Some(&[Rectangle::from_size(buf_size)]))
            .map_err(|e| format!("secondary import shared dmabuf: {e}"))?;
        Ok(Self {
            size,
            backing: BridgeBacking::Dma {
                _bo: bo,
                dmabuf,
                last_read_sync: None,
            },
            damage_tracker: OutputDamageTracker::new(size, 1.0, Transform::Normal),
            damage_bag: DamageBag::new(4),
            element_id: Id::new(),
            initialized: false,
        })
    }

    fn new_cpu(
        size: (i32, i32),
        primary_renderer: &mut GlesRenderer,
        sec_renderer: &mut GlesRenderer,
    ) -> Result<Self, String> {
        let buf_size = Size::<i32, BufferCoord>::from(size);
        let offscreen: GlesTexture = primary_renderer
            .create_buffer(Fourcc::Abgr8888, buf_size)
            .map_err(|e| format!("primary offscreen {}x{}: {e}", size.0, size.1))?;
        let sec_texture: GlesTexture = sec_renderer
            .create_buffer(Fourcc::Abgr8888, buf_size)
            .map_err(|e| format!("secondary texture {}x{}: {e}", size.0, size.1))?;
        Ok(Self {
            size,
            backing: BridgeBacking::Cpu { offscreen, sec_texture },
            damage_tracker: OutputDamageTracker::new(size, 1.0, Transform::Normal),
            damage_bag: DamageBag::new(4),
            element_id: Id::new(),
            initialized: false,
        })
    }

    fn reset(&mut self) {
        self.initialized = false;
        if let BridgeBacking::Dma { last_read_sync, .. } = &mut self.backing {
            *last_read_sync = None;
        }
    }

    fn is_dma(&self) -> bool {
        matches!(self.backing, BridgeBacking::Dma { .. })
    }
}

pub enum SecondaryRenderResult {
    Empty,
    Rendered { states: RenderElementStates },
}

/// A video card other than the rendering one: drives its own connectors natively
pub struct Gpu {
    pub manager: GbmDrmOutputManager,
    /// This card's own GLES renderer (`None` only for display-only cards without EGL/3D)
    pub renderer: Option<GlesRenderer>,
    pub driver: String,
    pub copy_strategy: CopyStrategy,
    pub device: u64,
    pub path: PathBuf,
    token: RegistrationToken,
}
pub struct Head {
    pub output: GbmDrmOutput,
    /// The card this connector belongs to (`dev_t`): CRTC handles repeat between cards
    pub device: u64,
    pub fd: DrmDeviceFd,
    pub connector: connector::Handle,
    pub crtc: crtc::Handle,
    pub name: String,
    pub size: (i32, i32),
    pub refresh: i32,
    pub physical_mm: (i32, i32),
    pub modes: Vec<smithay::reexports::drm::control::Mode>,
    pub pending: bool,
    pub protected: bool,
    pub queued_protected: bool,
    pub vrr_supported: bool,
    pub vrr_enabled: bool,
    /// Something changed since this head's last frame. Heads render at their own pace: a
    /// slower one waiting for its vblank never holds back the others.
    pub dirty: bool,
    /// Not before this: the frame is composed just ahead of the next vblank, so a client
    /// frame committed meanwhile still makes it (see `render_deadline`)
    pub render_at: Option<std::time::Instant>,
    /// Offscreen primary→secondary frame bridge (only created for heads on a secondary GPU)
    pub bridge: Option<SecondaryBridge>,
}
impl Head {
    pub fn set_vrr(&mut self, want: bool) {
        if !self.vrr_supported || self.vrr_enabled == want {
            return;
        }
        if let Err(err) = self.output.with_compositor(|c| c.use_vrr(want)) {
            tracing::debug!("DRM {}: VRR({want}) недоступен: {err:?}", self.name);
        }
        self.vrr_enabled = self.output.with_compositor(|c| c.vrr_enabled());
    }

    pub fn reset_bridge(&mut self) {
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.reset();
        }
    }
}
pub struct DrmBackend {
    // Drop heads (DrmOutput + SecondaryBridge textures/dmabufs) before extra/manager/renderer/gbm.
    pub heads: Vec<Head>,
    pub session: LibSeatSession,
    pub renderer: GlesRenderer,
    pub manager: GbmDrmOutputManager,
    pub extra: Vec<Gpu>,
    /// Cards that appeared (`Some(path)`) or went away since the last `flush_hotplug`
    pub gpu_events: Vec<(u64, Option<PathBuf>)>,
    pub device: u64,
    pub driver: String,
    pub gbm: GbmDevice<DrmDeviceFd>,
    handle: LoopHandle<'static, FeatherCompositor>,
    pub libinput: Libinput,
    pub render_node: Option<DrmNode>,
    pub active: bool,
    pub hotplug_pending: bool,
    pub fd: DrmDeviceFd,
    // Primary metadata, retained for the existing mode and scale controls.
    pub size: (i32, i32),
    pub refresh: i32,
    pub physical_mm: (i32, i32),
    pub connector_name: String,
    pub modes: Vec<smithay::reexports::drm::control::Mode>,
}
fn pick_gpu(seat: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = std::env::var_os("FEATHER_DRM_DEVICE") {
        let path = PathBuf::from(path);
        let card = path.canonicalize()
            .map_err(|e| format!("FEATHER_DRM_DEVICE={}: {e}", path.display()))?;
        if !all_gpus(seat)?.contains(&card) {
            return Err(format!(
                "FEATHER_DRM_DEVICE={}: expected a DRM card on seat {seat}",
                path.display()
            ).into());
        }
        return Ok(card);
    }
    primary_gpu(seat)
        .ok()
        .flatten()
        .or_else(|| all_gpus(seat).ok().and_then(|g| g.into_iter().next()))
        .ok_or_else(|| "видеокарта не найдена".into())
}
fn create_high_priority_egl_context(
    egl: &EGLDisplay,
    label: &str,
) -> Result<EGLContext, smithay::backend::egl::Error> {
    match EGLContext::new_with_priority(egl, ContextPriority::High) {
        Ok(ctx) => {
            tracing::info!("DRM ({label}): high-priority EGL context initialized");
            Ok(ctx)
        }
        Err(err) => {
            tracing::warn!(
                "DRM ({label}): high-priority EGL context unavailable ({err}), falling back to default"
            );
            EGLContext::new(egl)
        }
    }
}

fn driver_name(fd: &DrmDeviceFd) -> String {
    DrmCoreDevice::get_driver(fd)
        .ok()
        .map(|d| d.name().to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into())
}

/// Choose explicit DMA-BUF `(fourcc, modifiers)` candidates shared between primary rendering
/// and secondary texture import. Never returns `Modifier::Invalid`.
fn select_dma_candidates(
    primary_driver: &str,
    sec_driver: &str,
    mode: MultiGpuCopyMode,
    primary_render: &FormatSet,
    sec_import: &FormatSet,
) -> Result<Vec<(Fourcc, Vec<Modifier>)>, &'static str> {
    if mode == MultiGpuCopyMode::Cpu {
        return Err("FEATHER_MULTIGPU_COPY=cpu");
    }
    // Never import an NVIDIA-allocated VRAM dmabuf into a non-NVIDIA GPU in Auto mode:
    // on hybrid laptops (e.g. nvidia-drm -> amdgpu) amdgpu_cs_ioctl can reject the foreign
    // VRAM BO with -ENOMEM (-12) and Mesa aborts the compositor inside dri_create_fence_fd.
    if mode == MultiGpuCopyMode::Auto && primary_driver == "nvidia-drm" && sec_driver != "nvidia-drm" {
        return Err("nvidia-drm primary -> non-NVIDIA secondary uses CPU copy");
    }
    let same_vendor = primary_driver == sec_driver && primary_driver != "unknown";
    let mut out = Vec::new();
    for fourcc in COLOR_FORMATS {
        let mut mods: Vec<Modifier> = primary_render
            .iter()
            .filter(|f| f.code == fourcc && f.modifier != Modifier::Invalid && sec_import.contains(f))
            .map(|f| f.modifier)
            .collect();
        if mods.is_empty() {
            continue;
        }
        let has_linear = mods.contains(&Modifier::Linear);
        if !same_vendor {
            if has_linear {
                out.push((fourcc, vec![Modifier::Linear]));
            }
        } else {
            // Prefer Linear first on same-vendor multi-GPU too so differing GPU generations
            // or PCIe transfers do not trip over tiling/compression quirks.
            if has_linear {
                out.push((fourcc, vec![Modifier::Linear]));
            }
            mods.retain(|m| *m != Modifier::Linear);
            if !mods.is_empty() {
                out.push((fourcc, mods));
            }
        }
    }
    if out.is_empty() {
        return Err("no common explicit DMA-BUF format/modifier");
    }
    Ok(out)
}

fn probe_pixels_match(pixels: &[u8], width: usize, height: usize, check_alpha: bool) -> Result<(), String> {
    let check = |x: usize, y: usize, exp: [u8; 4], label: &str| -> Result<(), String> {
        let i = (y * width + x) * 4;
        if i + 4 > pixels.len() {
            return Err(format!("{label} pixel out of bounds"));
        }
        let got = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
        let channels = if check_alpha { 4 } else { 3 };
        let ok = got[..channels]
            .iter()
            .zip(exp[..channels].iter())
            .all(|(&a, &b)| a.abs_diff(b) <= 12);
        if ok {
            Ok(())
        } else {
            Err(format!("{label} pixel mismatch: got {got:?}, expected ~{exp:?}"))
        }
    };
    let cx = width / 2;
    let top_y = height / 4;
    let bot_y = (height * 3) / 4;
    check(cx, top_y, [64, 128, 191, 255], "top")?;
    check(cx, bot_y, [191, 64, 128, 255], "bottom")?;
    Ok(())
}

/// End-to-end 64x64 probe proving that a Dmabuf allocated & rendered on the primary GPU
/// imports cleanly as a texture on the secondary GPU with valid fence sync, colors, and orientation.
/// This catches silent GL_INVALID_OPERATION errors from EGLImageTargetTexture2DOES.
fn probe_dma_copy(
    primary_renderer: &mut GlesRenderer,
    primary_gbm: &GbmDevice<DrmDeviceFd>,
    sec_renderer: &mut GlesRenderer,
    fourcc: Fourcc,
    modifiers: &[Modifier],
) -> Result<(), String> {
    const PROBE_SIZE: i32 = 64;
    let buf_size = Size::<i32, BufferCoord>::from((PROBE_SIZE, PROBE_SIZE));
    let phys_size = Size::<i32, Physical>::from((PROBE_SIZE, PROBE_SIZE));
    let full = Rectangle::from_size(phys_size);
    let top_half = Rectangle::new((0, 0).into(), (PROBE_SIZE, PROBE_SIZE / 2).into());
    let bot_half = Rectangle::new((0, PROBE_SIZE / 2).into(), (PROBE_SIZE, PROBE_SIZE / 2).into());

    let mut alloc = GbmAllocator::new(primary_gbm.clone(), GbmBufferFlags::RENDERING);
    let bo = alloc
        .create_buffer(PROBE_SIZE as u32, PROBE_SIZE as u32, fourcc, modifiers)
        .map_err(|e| format!("allocate probe bo: {e}"))?;
    let mut dmabuf = bo.export().map_err(|e| format!("export probe dmabuf: {e}"))?;
    if dmabuf.format().modifier == Modifier::Invalid {
        return Err("probe dmabuf fell back to implicit modifier".into());
    }

    let sync = {
        let mut fb = primary_renderer
            .bind(&mut dmabuf)
            .map_err(|e| format!("primary bind: {e}"))?;
        let mut frame = primary_renderer
            .render(&mut fb, phys_size, Transform::Normal)
            .map_err(|e| format!("primary render: {e}"))?;
        frame
            .clear(Color32F::new(0.25, 0.50, 0.75, 1.0), &[top_half])
            .map_err(|e| format!("primary clear top: {e}"))?;
        frame
            .clear(Color32F::new(0.75, 0.25, 0.50, 1.0), &[bot_half])
            .map_err(|e| format!("primary clear bottom: {e}"))?;
        frame.finish().map_err(|e| format!("primary finish: {e}"))?
    };

    let imported = sec_renderer
        .import_dmabuf(&dmabuf, Some(&[Rectangle::from_size(buf_size)]))
        .map_err(|e| format!("secondary import_dmabuf: {e}"))?;
    let mut dst: GlesTexture = sec_renderer
        .create_buffer(Fourcc::Abgr8888, buf_size)
        .map_err(|e| format!("secondary create_buffer: {e}"))?;
    {
        let mut fb = sec_renderer
            .bind(&mut dst)
            .map_err(|e| format!("secondary bind: {e}"))?;
        let mut frame = sec_renderer
            .render(&mut fb, phys_size, Transform::Normal)
            .map_err(|e| format!("secondary render: {e}"))?;
        frame
            .wait(&sync)
            .map_err(|e| format!("secondary wait sync: {e}"))?;
        frame
            .clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &[full])
            .map_err(|e| format!("secondary clear: {e}"))?;
        frame
            .render_texture_from_to(
                &imported,
                Rectangle::from_size((PROBE_SIZE as f64, PROBE_SIZE as f64).into()),
                full,
                &[full],
                &[full],
                Transform::Normal,
                1.0,
                None,
                &[],
            )
            .map_err(|e| format!("secondary draw: {e}"))?;
        let dst_sync = frame.finish().map_err(|e| format!("secondary finish: {e}"))?;
        let _ = dst_sync.wait();
    }
    let mapping = sec_renderer
        .copy_texture(&dst, Rectangle::from_size(buf_size), Fourcc::Abgr8888)
        .map_err(|e| format!("secondary copy_texture: {e}"))?;
    let result = sec_renderer
        .map_texture(&mapping)
        .map_err(|e| format!("secondary map_texture: {e}"))
        .and_then(|pixels| {
            probe_pixels_match(
                pixels,
                PROBE_SIZE as usize,
                PROBE_SIZE as usize,
                fourcc == Fourcc::Argb8888,
            )
        });

    drop(mapping);
    drop(dst);
    drop(imported);
    drop(dmabuf);
    drop(bo);
    let _ = sec_renderer.cleanup_texture_cache();
    let _ = primary_renderer.cleanup_texture_cache();
    result
}

fn choose_copy_strategy(
    primary_driver: &str,
    sec_driver: &str,
    mode: MultiGpuCopyMode,
    primary_renderer: &mut GlesRenderer,
    primary_gbm: &GbmDevice<DrmDeviceFd>,
    sec_renderer: &mut GlesRenderer,
) -> (CopyStrategy, String) {
    let primary_render = primary_renderer.egl_context().dmabuf_render_formats().clone();
    let sec_import = sec_renderer.dmabuf_formats();
    let candidates = match select_dma_candidates(primary_driver, sec_driver, mode, &primary_render, &sec_import) {
        Ok(c) => c,
        Err(reason) => return (CopyStrategy::CpuCopy, reason.to_string()),
    };
    let mut last_err = String::new();
    for (fourcc, modifiers) in candidates {
        match probe_dma_copy(primary_renderer, primary_gbm, sec_renderer, fourcc, &modifiers) {
            Ok(()) => {
                let desc = format!("GPU DMA-BUF ({fourcc:?}, {modifiers:?})");
                return (CopyStrategy::DmaCopy { fourcc, modifiers }, desc);
            }
            Err(e) => {
                tracing::debug!(
                    "DRM multi-GPU DMA probe failed ({primary_driver} -> {sec_driver}, {fourcc:?}, {modifiers:?}): {e}"
                );
                last_err = e;
            }
        }
    }
    (CopyStrategy::CpuCopy, format!("DMA-BUF проба не прошла: {last_err}"))
}

fn clamp_damage_rects(
    phys_damage: &[Rectangle<i32, Physical>],
    bounds: Size<i32, BufferCoord>,
) -> Vec<Rectangle<i32, BufferCoord>> {
    let limit = Rectangle::from_size(bounds);
    phys_damage
        .iter()
        .filter_map(|r| {
            Rectangle::<i32, BufferCoord>::new((r.loc.x, r.loc.y).into(), (r.size.w, r.size.h).into())
                .intersection(limit)
        })
        .filter(|r| r.size.w > 0 && r.size.h > 0)
        .collect()
}

/// Clamp, deduplicate, and merge overlapping/touching damage rects for CPU readback.
/// If more than `MAX_CPU_COPY_RECTS` disjoint rects remain, collapses them to their tight
/// bounding box (rather than the whole screen) to keep readback tiny for localized updates.
fn merge_copy_rects(
    phys_damage: &[Rectangle<i32, Physical>],
    bounds: Size<i32, BufferCoord>,
) -> Vec<Rectangle<i32, BufferCoord>> {
    let mut merged: Vec<Rectangle<i32, BufferCoord>> = Vec::new();
    for mut rect in clamp_damage_rects(phys_damage, bounds) {
        let mut i = 0;
        while i < merged.len() {
            if merged[i].overlaps_or_touches(rect) {
                rect = rect.merge(merged.swap_remove(i));
                i = 0;
            } else {
                i += 1;
            }
        }
        merged.push(rect);
    }
    if merged.len() > MAX_CPU_COPY_RECTS {
        if let Some(bbox) = merged.into_iter().reduce(|a, b| a.merge(b)) {
            return vec![bbox];
        }
        return Vec::new();
    }
    merged
}

/// Compose `elements` for a secondary-GPU `head` on the primary GPU and transfer the damaged
/// pixels into the secondary GPU's native `DrmOutput` via verified DMA-BUF or CPU damage copy.
pub fn render_secondary_head(
    head: &mut Head,
    primary_renderer: &mut GlesRenderer,
    primary_gbm: &GbmDevice<DrmDeviceFd>,
    sec: &mut Gpu,
    elements: &[OutputElement],
    clear_color: Color32F,
) -> Result<SecondaryRenderResult, String> {
    let Some(sec_renderer) = sec.renderer.as_mut() else {
        return Err("secondary GPU has no EGL renderer".into());
    };
    let size = head.size;
    let want_dma = matches!(sec.copy_strategy, CopyStrategy::DmaCopy { .. });
    let need_new = head
        .bridge
        .as_ref()
        .is_none_or(|b| b.size != size || b.is_dma() != want_dma);
    if need_new {
        head.bridge = None;
        let bridge = match &sec.copy_strategy {
            CopyStrategy::DmaCopy { fourcc, modifiers } => {
                match SecondaryBridge::new_dma(size, primary_gbm, primary_renderer, sec_renderer, *fourcc, modifiers) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(
                            "DRM {} ({}): не удалось создать DMA-BUF мост ({e}), переключаюсь на CPU-копирование",
                            head.name,
                            sec.path.display(),
                        );
                        sec.copy_strategy = CopyStrategy::CpuCopy;
                        SecondaryBridge::new_cpu(size, primary_renderer, sec_renderer)?
                    }
                }
            }
            CopyStrategy::CpuCopy => SecondaryBridge::new_cpu(size, primary_renderer, sec_renderer)?,
        };
        head.bridge = Some(bridge);
    }

    let first_attempt = render_secondary_with_bridge(head, primary_renderer, sec_renderer, elements, clear_color);
    match first_attempt {
        Ok(res) => Ok(res),
        Err(e) if head.bridge.as_ref().is_some_and(SecondaryBridge::is_dma) => {
            tracing::warn!(
                "DRM {} ({}): сбой GPU DMA-BUF копирования ({e}), переключаюсь на CPU-копирование",
                head.name,
                sec.path.display(),
            );
            sec.copy_strategy = CopyStrategy::CpuCopy;
            head.bridge = None;
            let _ = sec_renderer.cleanup_texture_cache();
            let _ = primary_renderer.cleanup_texture_cache();
            head.bridge = Some(SecondaryBridge::new_cpu(size, primary_renderer, sec_renderer)?);
            render_secondary_with_bridge(head, primary_renderer, sec_renderer, elements, clear_color)
        }
        Err(e) => Err(e),
    }
}

fn upload_cpu_patches(
    primary_renderer: &mut GlesRenderer,
    sec_renderer: &mut GlesRenderer,
    sec_texture: &mut GlesTexture,
    phys_size: Size<i32, Physical>,
    mappings: Vec<(smithay::backend::renderer::gles::GlesMapping, Rectangle<i32, BufferCoord>)>,
) -> Result<(), String> {
    let mut patches = Vec::with_capacity(mappings.len());
    for (mapping, rect) in &mappings {
        let slice = primary_renderer
            .map_texture(mapping)
            .map_err(|e| format!("primary map_texture: {e}"))?;
        let patch = sec_renderer
            .import_memory(slice, Fourcc::Abgr8888, rect.size, false)
            .map_err(|e| format!("secondary import_memory: {e}"))?;
        patches.push((patch, *rect));
    }
    drop(mappings);
    let _ = primary_renderer.cleanup_texture_cache();
    {
        let mut sec_fb = sec_renderer
            .bind(sec_texture)
            .map_err(|e| format!("secondary bind staging texture: {e}"))?;
        let mut frame = sec_renderer
            .render(&mut sec_fb, phys_size, Transform::Normal)
            .map_err(|e| format!("secondary render staging: {e}"))?;
        for (patch, rect) in &patches {
            let dst = Rectangle::<i32, Physical>::new(
                (rect.loc.x, rect.loc.y).into(),
                (rect.size.w, rect.size.h).into(),
            );
            let src = Rectangle::<f64, BufferCoord>::from_size(
                (rect.size.w as f64, rect.size.h as f64).into(),
            );
            let rel = [Rectangle::from_size(dst.size)];
            frame
                .render_texture_from_to(patch, src, dst, &rel, &rel, Transform::Normal, 1.0, None, &[])
                .map_err(|e| format!("secondary blit patch: {e}"))?;
        }
        let _ = frame.finish().map_err(|e| format!("secondary finish staging: {e}"))?;
    }
    drop(patches);
    let _ = sec_renderer.cleanup_texture_cache();
    Ok(())
}

fn render_secondary_with_bridge(
    head: &mut Head,
    primary_renderer: &mut GlesRenderer,
    sec_renderer: &mut GlesRenderer,
    elements: &[OutputElement],
    clear_color: Color32F,
) -> Result<SecondaryRenderResult, String> {
    let bridge = head.bridge.as_mut().ok_or_else(|| "bridge missing".to_string())?;
    let size = bridge.size;
    let buf_size = Size::<i32, BufferCoord>::from(size);
    let phys_size = Size::<i32, Physical>::from(size);
    let age = usize::from(bridge.initialized);

    let (primary_states, tex, buf_damage) = match &mut bridge.backing {
        BridgeBacking::Dma {
            dmabuf,
            last_read_sync,
            ..
        } => {
            if let Some(prev) = last_read_sync.take() {
                if !prev.is_reached() {
                    let _ = primary_renderer.wait(&prev);
                }
            }
            let (primary_sync, primary_states, mut buf_damage) = {
                let mut fb = primary_renderer
                    .bind(dmabuf)
                    .map_err(|e| format!("primary bind dmabuf: {e}"))?;
                let res = bridge
                    .damage_tracker
                    .render_output(primary_renderer, &mut fb, age, elements, clear_color)
                    .map_err(|e| format!("primary render: {e:?}"))?;
                let Some(phys_damage) = res.damage else {
                    return Ok(SecondaryRenderResult::Empty);
                };
                let dmg = clamp_damage_rects(phys_damage, buf_size);
                (res.sync, res.states, dmg)
            };
            if buf_damage.is_empty() {
                if !bridge.initialized {
                    buf_damage.push(Rectangle::from_size(buf_size));
                } else {
                    return Ok(SecondaryRenderResult::Empty);
                }
            }
            sec_renderer
                .wait(&primary_sync)
                .map_err(|e| format!("secondary wait sync: {e}"))?;
            let tex = sec_renderer
                .import_dmabuf(dmabuf, Some(&buf_damage))
                .map_err(|e| format!("secondary import_dmabuf: {e}"))?;
            (primary_states, tex, buf_damage)
        }
        BridgeBacking::Cpu {
            offscreen,
            sec_texture,
        } => {
            let (primary_states, copy_rects, mappings) = {
                let mut fb = primary_renderer
                    .bind(offscreen)
                    .map_err(|e| format!("primary bind offscreen: {e}"))?;
                let res = bridge
                    .damage_tracker
                    .render_output(primary_renderer, &mut fb, age, elements, clear_color)
                    .map_err(|e| format!("primary render offscreen: {e:?}"))?;
                let _ = res.sync;
                let Some(phys_damage) = res.damage else {
                    return Ok(SecondaryRenderResult::Empty);
                };
                let mut rects = merge_copy_rects(phys_damage, buf_size);
                if rects.is_empty() {
                    if !bridge.initialized {
                        rects.push(Rectangle::from_size(buf_size));
                    } else {
                        return Ok(SecondaryRenderResult::Empty);
                    }
                }
                let mut mappings = Vec::with_capacity(rects.len());
                for &rect in &rects {
                    let mapping = primary_renderer
                        .copy_framebuffer(&fb, rect, Fourcc::Abgr8888)
                        .map_err(|e| format!("primary copy_framebuffer: {e}"))?;
                    mappings.push((mapping, rect));
                }
                (res.states, rects, mappings)
            };
            upload_cpu_patches(primary_renderer, sec_renderer, sec_texture, phys_size, mappings)?;
            (primary_states, sec_texture.clone(), copy_rects)
        }
    };

    bridge.initialized = true;
    bridge.damage_bag.add(buf_damage);
    let elem = TextureRenderElement::from_texture_with_damage(
        bridge.element_id.clone(),
        sec_renderer.context_id(),
        (0.0, 0.0),
        tex,
        1,
        Transform::Normal,
        Some(1.0),
        None,
        None,
        Some(vec![Rectangle::from_size(buf_size)]),
        bridge.damage_bag.snapshot(),
        Kind::Unspecified,
    );

    let sec_elements = [elem];
    let r = head
        .output
        .render_frame(sec_renderer, &sec_elements, clear_color, FrameFlags::empty())
        .map_err(|e| format!("secondary output render_frame: {e:?}"))?;
    if r.is_empty {
        return Ok(SecondaryRenderResult::Empty);
    }
    if r.needs_sync() {
        if let PrimaryPlaneElement::Swapchain(e) = &r.primary_element {
            let _ = e.sync.wait();
        }
    }
    if let BridgeBacking::Dma { last_read_sync, .. } = &mut bridge.backing {
        if let PrimaryPlaneElement::Swapchain(e) = &r.primary_element {
            *last_read_sync = Some(e.sync.clone());
        }
    }
    Ok(SecondaryRenderResult::Rendered { states: primary_states })
}

impl DrmBackend {
    pub fn sync_primary(&mut self, name: &str) {
        if let Some(h) = self.heads.iter().find(|h| h.name == name) {
            self.size = h.size;
            self.refresh = h.refresh;
            self.physical_mm = h.physical_mm;
            self.connector_name = h.name.clone();
            self.modes = h.modes.clone();
        }
    }
    /// Change resolution/refresh of head `name` using the renderer that owns its GPU.
    pub fn set_head_mode(
        &mut self,
        name: &str,
        mode: smithay::reexports::drm::control::Mode,
    ) -> Result<(), String> {
        let Some(head) = self.heads.iter_mut().find(|h| h.name == name) else {
            return Err("Выход больше недоступен".into());
        };
        let renderer = if head.device == self.device {
            &mut self.renderer
        } else {
            self.extra
                .iter_mut()
                .find(|g| g.device == head.device)
                .and_then(|g| g.renderer.as_mut())
                .unwrap_or(&mut self.renderer)
        };
        head.output
            .use_mode::<_, crate::render::OutputElement>(
                mode,
                renderer,
                &DrmOutputRenderElements::default(),
            )
            .map_err(|e| format!("Не удалось применить режим: {e:?}"))?;
        let info = mode_info(mode);
        let size = (info.width, info.height);
        self.size = size;
        self.refresh = info.refresh;
        head.size = size;
        head.refresh = info.refresh;
        head.output.with_compositor(|c| {
            c.set_output_mode_source(OutputModeSource::Static {
                size: size.into(),
                scale: 1.0.into(),
                transform: Transform::Normal,
            })
        });
        head.output.reset_buffers();
        head.bridge = None;
        head.pending = false;
        head.protected = false;
        Ok(())
    }
    pub fn clear_all(&mut self) -> Result<(), String> {
        let mut error = None;
        for h in &mut self.heads {
            match h.output.with_compositor(|c| c.clear()) {
                Ok(()) => {
                    h.pending = false;
                    h.protected = true;
                    h.reset_bridge();
                }
                Err(e) => error = Some(format!("{}: {e}", h.name)),
            }
        }
        error.map_or(Ok(()), Err)
    }
    pub fn specs(&self) -> Vec<(String, crate::system_settings::Mode, (i32, i32))> {
        self.heads
            .iter()
            .map(|h| {
                (
                    h.name.clone(),
                    crate::system_settings::Mode {
                        width: h.size.0,
                        height: h.size.1,
                        refresh: h.refresh,
                    },
                    h.physical_mm,
                )
            })
            .collect()
    }
    /// Every card is scanned again; a card that fails keeps the others working
    pub fn rescan(
        &mut self,
        saved: Option<&crate::settings::Display>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let primary = scan(&mut self.manager, &mut self.renderer, &mut self.heads, self.device, &self.fd, saved);
        for g in &mut self.extra {
            let fd = g.manager.device().device_fd().clone();
            let renderer = g.renderer.as_mut().unwrap_or(&mut self.renderer);
            if let Err(e) = scan(&mut g.manager, renderer, &mut self.heads, g.device, &fd, saved) {
                tracing::warn!("DRM {}: {e}", g.path.display());
            }
        }
        primary
    }
    /// Opens another card for its connectors.
    /// When the card supports EGL/GLES (NVIDIA, AMD, Intel), it gets its own `GlesRenderer`
    /// and native scanout swapchain so it never forces foreign VRAM buffers into the primary
    /// GPU's command stream; composed frames are copied via verified DMA-BUF or CPU damage copy.
    /// Display-only cards without EGL fall back to verified primary-allocated linear scanout.
    fn open_secondary(&mut self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let fd = self.session.open(path, OPEN_FLAGS).map_err(|e| format!("{}: {e}", path.display()))?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let device = DrmNode::from_file(&fd)?.dev_id() as u64;
        if device == self.device || self.extra.iter().any(|g| g.device == device) {
            return Ok(());
        }
        let driver = driver_name(&fd);
        tracing::info!("DRM: probing secondary card {} (driver {driver})", path.display());
        let (drm, notifier) = DrmDevice::new(fd.clone(), true)?;
        let gbm = GbmDevice::new(fd.clone())?;

        let sec_renderer = (|| -> Result<GlesRenderer, Box<dyn std::error::Error>> {
            let egl = unsafe { EGLDisplay::new(gbm.clone())? };
            let context = create_high_priority_egl_context(&egl, &driver)?;
            let renderer = unsafe { GlesRenderer::new(context)? };
            Ok(renderer)
        })();

        let (manager, renderer, copy_strategy, desc) = match sec_renderer {
            Ok(mut sec_renderer) => {
                let formats = sec_renderer.egl_context().dmabuf_render_formats().clone();
                let manager = GbmDrmOutputManager::new(
                    drm,
                    GbmAllocator::new(
                        gbm.clone(),
                        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
                    ),
                    GbmFramebufferExporter::new(gbm, None),
                    None,
                    COLOR_FORMATS,
                    formats,
                );
                let mode = multigpu_copy_mode();
                let (strategy, detail) = choose_copy_strategy(
                    &self.driver,
                    &driver,
                    mode,
                    &mut self.renderer,
                    &self.gbm,
                    &mut sec_renderer,
                );
                let desc = match &strategy {
                    CopyStrategy::DmaCopy { .. } => format!("свой EGL, {detail}"),
                    CopyStrategy::CpuCopy => format!("свой EGL, CPU-копирование повреждений ({detail})"),
                };
                (manager, Some(sec_renderer), strategy, desc)
            }
            Err(egl_err) => {
                tracing::warn!(
                    "DRM {}: EGL недоступен ({egl_err}), пробую линейный вывод буферов основной карты",
                    path.display(),
                );
                if self.driver == "nvidia-drm" {
                    return Err(format!("EGL unavailable on secondary card ({egl_err}) and primary is nvidia-drm").into());
                }
                let linear: Vec<_> = self
                    .renderer
                    .egl_context()
                    .dmabuf_render_formats()
                    .iter()
                    .filter(|f| f.modifier == Modifier::Linear)
                    .copied()
                    .collect();
                if linear.is_empty() {
                    return Err(format!("EGL unavailable ({egl_err}) and primary has no linear render formats").into());
                }
                let mut primary_alloc = GbmAllocator::new(self.gbm.clone(), GbmBufferFlags::RENDERING);
                let exporter = GbmFramebufferExporter::new(gbm, None);
                let probe_bo = primary_alloc
                    .create_buffer(64, 64, COLOR_FORMATS[0], &[Modifier::Linear])
                    .map_err(|e| format!("probe primary linear bo: {e}"))?;
                exporter
                    .add_framebuffer(&fd, ExportBuffer::Allocator(&probe_bo), false)
                    .map_err(|e| format!("secondary cannot import primary linear framebuffer: {e}"))?;
                drop(probe_bo);
                let manager = GbmDrmOutputManager::new(
                    drm,
                    primary_alloc,
                    exporter,
                    None,
                    COLOR_FORMATS,
                    linear,
                );
                (
                    manager,
                    None,
                    CopyStrategy::CpuCopy,
                    "без EGL, линейный вывод буферов основной карты".into(),
                )
            }
        };

        let token = self.handle.insert_source(notifier, move |event, metadata, state| match event {
            DrmEvent::VBlank(crtc) => state.on_drm_vblank(device, crtc, metadata.take()),
            DrmEvent::Error(e) => tracing::warn!("DRM: {e}"),
        })?;
        tracing::info!(
            "DRM: вторая видеокарта {} (driver {driver}, {desc})",
            path.display(),
        );
        self.extra.push(Gpu {
            manager,
            renderer,
            driver,
            copy_strategy,
            device,
            path: path.to_path_buf(),
            token,
        });
        Ok(())
    }
    /// Cards plugged in or removed while running (docks, external GPUs)
    fn apply_gpu_events(&mut self) {
        for (device, path) in std::mem::take(&mut self.gpu_events) {
            match path {
                Some(path) => {
                    if let Err(e) = self.open_secondary(&path) {
                        tracing::warn!("DRM: видеокарта {} не подключена: {e}", path.display());
                    }
                }
                None => {
                    let Some(i) = self.extra.iter().position(|g| g.device == device) else { continue };
                    self.heads.retain(|h| h.device != device);
                    let gone = self.extra.remove(i);
                    self.handle.remove(gone.token);
                    tracing::info!("DRM: видеокарта {} отключена", gone.path.display());
                }
            }
        }
    }
}
fn scan(
    manager: &mut GbmDrmOutputManager,
    renderer: &mut GlesRenderer,
    heads: &mut Vec<Head>,
    device: u64,
    fd: &DrmDeviceFd,
    saved: Option<&crate::settings::Display>,
) -> Result<(), Box<dyn std::error::Error>> {
    {
        let room = MAX_HEADS.saturating_sub(heads.iter().filter(|h| h.device != device).count());
        let drm = manager.device();
        let res = drm.resource_handles()?;
        let connectors: Vec<_> = res
            .connectors()
            .iter()
            .filter_map(|c| drm.get_connector(*c, true).ok())
            .filter(|c| c.state() == connector::State::Connected && !c.modes().is_empty())
            .take(room)
            .collect();
        let candidates: Vec<Vec<usize>> = connectors
            .iter()
            .map(|c| {
                let mut choices: Vec<_> = c
                    .encoders()
                    .iter()
                    .filter_map(|e| drm.get_encoder(*e).ok())
                    .flat_map(|e| res.filter_crtcs(e.possible_crtcs()))
                    .filter_map(|h| res.crtcs().iter().position(|v| *v == h))
                    .collect();
                choices.sort_unstable();
                choices.dedup();
                if let Some(existing) = heads
                    .iter()
                    .find(|h| h.device == device && h.connector == c.handle())
                    .and_then(|h| res.crtcs().iter().position(|v| *v == h.crtc))
                {
                    if let Some(i) = choices.iter().position(|n| *n == existing) {
                        choices.swap(0, i);
                    }
                }
                choices
            })
            .collect();
        let assignments = crate::monitors::assign_crtcs(&candidates, res.crtcs().len());
        // Drop disconnected/reassigned scanouts first, freeing their CRTCs and planes.
        heads.retain_mut(|h| {
            if h.device != device {
                return true;
            }
            let keep = connectors.iter().enumerate().any(|(i, c)| {
                c.handle() == h.connector
                    && assignments[i].is_some_and(|a| res.crtcs()[a] == h.crtc)
                    && c.modes().iter().any(|m| {
                        let mode = mode_info(*m);
                        (mode.width, mode.height) == h.size && mode.refresh == h.refresh
                    })
            });
            if !keep {
                let _ = h.output.with_compositor(|c| c.clear());
            }
            keep
        });
        for (i, info) in connectors.into_iter().enumerate() {
            let Some(assignment) = assignments[i] else {
                tracing::warn!("DRM: нет свободного CRTC для {:?}", info.handle());
                continue;
            };
            if let Some(h) = heads.iter_mut().find(|h| h.device == device && h.connector == info.handle()) {
                h.modes = info.modes().to_vec();
                h.physical_mm = info
                    .size()
                    .map(|(w, h)| (w as i32, h as i32))
                    .unwrap_or((0, 0));
                continue;
            }
            let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
            let preferred = info
                .modes()
                .iter()
                .copied()
                .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                .unwrap_or(info.modes()[0]);
            let mut mode = saved
                .filter(|s| s.name == name)
                .and_then(|s| info.modes().iter().find(|m| mode_info(**m) == s.mode))
                .copied()
                .unwrap_or(preferred);
            let crtc = res.crtcs()[assignment];
            let create = |mode: smithay::reexports::drm::control::Mode,
                          manager: &mut GbmDrmOutputManager,
                          renderer: &mut GlesRenderer| {
                let mode_info = mode_info(mode);
                tracing::info!(
                    "DRM: initializing {name} on card {device:#x}, {}x{} @ {} mHz",
                    mode_info.width, mode_info.height, mode_info.refresh,
                );
                manager.initialize_output::<_, OutputElement>(
                    crtc,
                    mode,
                    &[info.handle()],
                    OutputModeSource::Static {
                        size: (mode.size().0 as i32, mode.size().1 as i32).into(),
                        scale: 1.0.into(),
                        transform: Transform::Normal,
                    },
                    None,
                    renderer,
                    &DrmOutputRenderElements::default(),
                )
            };
            let output = match create(mode, manager, renderer) {
                Ok(o) => o,
                Err(e) if mode != preferred => {
                    tracing::warn!("DRM {name}: сохранённый режим недоступен: {e:?}");
                    mode = preferred;
                    match create(mode, manager, renderer) {
                        Ok(o) => o,
                        Err(e) => {
                            tracing::warn!("DRM {name}: {e:?}");
                            continue;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("DRM {name}: {e:?}");
                    continue;
                }
            };
            let vrr_supported = matches!(
                output.with_compositor(|c| c.vrr_supported(info.handle())),
                Ok(smithay::backend::drm::VrrSupport::Supported
                    | smithay::backend::drm::VrrSupport::RequiresModeset)
            );
            let vrr_enabled = output.with_compositor(|c| c.vrr_enabled());
            tracing::info!(
                "DRM: {name} {}x{} (vrr_supported={vrr_supported})",
                mode.size().0,
                mode.size().1
            );
            heads.push(Head {
                output,
                device,
                fd: fd.clone(),
                connector: info.handle(),
                crtc,
                name,
                size: (mode.size().0 as i32, mode.size().1 as i32),
                refresh: mode_info(mode).refresh,
                physical_mm: info
                    .size()
                    .map(|(w, h)| (w as i32, h as i32))
                    .unwrap_or((0, 0)),
                modes: info.modes().to_vec(),
                pending: false,
                protected: false,
                queued_protected: false,
                vrr_supported,
                vrr_enabled,
                dirty: true,
                render_at: None,
                bridge: None,
            });
        }
    }
    Ok(())
}
/// `FEATHER_SINGLE_GPU=1` — только основная видеокарта, как до поддержки нескольких
fn single_gpu() -> bool {
    std::env::var_os("FEATHER_SINGLE_GPU").is_some_and(|v| v != "0")
}
pub fn init(
    handle: &LoopHandle<'static, FeatherCompositor>,
    saved: Option<&crate::settings::Display>,
) -> Result<DrmBackend, Box<dyn std::error::Error>> {
    let (mut session, notifier) = LibSeatSession::new().map_err(|e| format!("libseat: {e}"))?;
    let seat = session.seat();
    let path = pick_gpu(&seat)?;
    let single_gpu = single_gpu();
    tracing::info!(
        "DRM: initializing EGL on {} (seat {seat}, single GPU: {single_gpu})",
        path.display(),
    );
    let fd = session
        .open(
            &path,
            OPEN_FLAGS,
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let driver = driver_name(&fd);
    tracing::info!(
        "DRM: primary card {} (driver {driver}, seat {seat}, single GPU: {single_gpu})",
        path.display(),
    );
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd.clone())?;
    let primary_gbm = gbm.clone();
    let egl = unsafe { EGLDisplay::new(gbm.clone())? };
    let context = create_high_priority_egl_context(&egl, &driver)?;
    let renderer = unsafe { GlesRenderer::new(context)? };
    let node = DrmNode::from_file(&fd).ok();
    let device = node.map(|n| n.dev_id() as u64).unwrap_or(0);
    let render_node = node.and_then(|n| n.node_with_type(NodeType::Render).and_then(Result::ok));
    let formats = renderer.egl_context().dmabuf_render_formats().clone();
    let manager = GbmDrmOutputManager::new(
        drm,
        GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        ),
        GbmFramebufferExporter::new(gbm.clone(), render_node),
        Some(gbm.clone()),
        COLOR_FORMATS,
        formats,
    );
    let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
    libinput
        .udev_assign_seat(&seat)
        .map_err(|_| "libinput: seat")?;
    handle.insert_source(
        LibinputInputBackend::new(libinput.clone()),
        |mut event, _, state| {
            match &mut event {
                smithay::backend::input::InputEvent::DeviceAdded { device } => {
                    state.configure_input_device(device);
                    state.input_devices.push(device.clone());
                }
                smithay::backend::input::InputEvent::DeviceRemoved { device } => {
                    state.input_devices.retain(|d| d != device)
                }
                _ => {}
            }
            let started = std::time::Instant::now();
            state.handle_input(event);
            state.slow_steps.note(started.elapsed(), format_args!("событие ввода"));
        },
    )?;
    handle.insert_source(drm_notifier, move |event, metadata, state| match event {
        DrmEvent::VBlank(crtc) => state.on_drm_vblank(device, crtc, metadata.take()),
        DrmEvent::Error(e) => tracing::warn!("DRM: {e}"),
    })?;
    handle.insert_source(notifier, |event, _, state| match event {
        SessionEvent::PauseSession => state.on_session_pause(),
        SessionEvent::ActivateSession => state.on_session_activate(),
    })?;
    handle.insert_source(UdevBackend::new(&seat)?, move |event, _, state| {
        let crate::backend::Backend::Drm(d) = &mut state.backend else { return };
        match event {
            UdevEvent::Added { device_id, path } if !single_gpu => d.gpu_events.push((device_id as u64, Some(path))),
            UdevEvent::Removed { device_id } => d.gpu_events.push((device_id as u64, None)),
            _ => {}
        }
        d.hotplug_pending = true;
    })?;
    let mut backend = DrmBackend {
        heads: Vec::new(),
        session,
        renderer,
        manager,
        extra: Vec::new(),
        gpu_events: Vec::new(),
        device,
        driver,
        gbm: primary_gbm,
        handle: handle.clone(),
        libinput,
        render_node,
        active: true,
        hotplug_pending: false,
        fd,
        size: (640, 480),
        refresh: 60000,
        physical_mm: (0, 0),
        connector_name: String::new(),
        modes: Vec::new(),
    };
    if !single_gpu {
        for other in all_gpus(&seat).unwrap_or_default().into_iter().filter(|p| *p != path) {
            if let Err(e) = backend.open_secondary(&other) {
                tracing::warn!("DRM: видеокарта {} не подключена: {e}", other.display());
            }
        }
    }
    backend.rescan(saved)?;
    let name = backend
        .heads
        .first()
        .ok_or("нет подключённого монитора")?
        .name
        .clone();
    backend.sync_primary(&name);
    let _ = backend.renderer.dmabuf_formats();
    Ok(backend)
}

pub fn mode_info(mode: smithay::reexports::drm::control::Mode) -> crate::system_settings::Mode {
    let flags = mode.flags();
    crate::system_settings::Mode {
        width: mode.size().0 as i32,
        height: mode.size().1 as i32,
        refresh: refresh_rate(
            mode.clock(),
            mode.hsync().2,
            mode.vsync().2,
            mode.vscan(),
            flags.contains(smithay::reexports::drm::control::ModeFlags::INTERLACE),
            flags.contains(smithay::reexports::drm::control::ModeFlags::DBLSCAN),
        )
        .unwrap_or(mode.vrefresh() as i32 * 1000),
    }
}
fn refresh_rate(
    clock: u32,
    htotal: u16,
    vtotal: u16,
    vscan: u16,
    interlaced: bool,
    double_scan: bool,
) -> Option<i32> {
    let mut numerator = clock as u64 * 1_000_000;
    let mut denominator = htotal as u64 * vtotal as u64;
    if denominator == 0 {
        return None;
    }
    if interlaced {
        numerator *= 2;
    }
    if double_scan {
        denominator *= 2;
    }
    if vscan > 1 {
        denominator *= vscan as u64;
    }
    Some(((numerator + denominator / 2) / denominator).min(i32::MAX as u64) as i32)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mode_timings_preserve_fractional_refresh_and_scan_flags() {
        assert_eq!(
            refresh_rate(148500, 2200, 1125, 1, false, false),
            Some(60000)
        );
        assert_eq!(
            refresh_rate(148352, 2200, 1125, 1, false, false),
            Some(59940)
        );
        assert_eq!(refresh_rate(74250, 2200, 1125, 1, true, false), Some(60000));
        assert_eq!(
            refresh_rate(148500, 2200, 1125, 2, false, false),
            Some(30000)
        );
        assert_eq!(refresh_rate(148500, 2200, 0, 1, false, false), None);
    }

    #[test]
    fn frames_are_composed_ahead_of_the_vblank_by_their_cost() {
        use std::time::Duration;
        let ms = Duration::from_millis;
        let hz60 = crate::render::refresh_period(60000).unwrap();
        assert_eq!(hz60.as_micros(), 16666);
        // A cheap frame waits until a quarter of the refresh is left
        assert_eq!(deadline_wait(hz60, Duration::from_micros(650)), Some(hz60 / 2 + hz60 / 4));
        // An expensive one starts as early as it needs: twice its cost plus a millisecond
        assert_eq!(deadline_wait(hz60, ms(4)), Some(hz60 - ms(9)));
        // At 144 Hz the 4 ms floor leaves under 3 ms of waiting
        let hz144 = crate::render::refresh_period(144000).unwrap();
        assert_eq!(deadline_wait(hz144, Duration::ZERO), Some(hz144 - ms(4)));
        // No time to wait: compose right away
        assert_eq!(deadline_wait(hz60, ms(8)), None);
        assert_eq!(deadline_wait(crate::render::refresh_period(240000).unwrap(), Duration::ZERO), None);
        assert_eq!(crate::render::refresh_period(0), None);
    }

    fn format_set(entries: &[(Fourcc, Modifier)]) -> FormatSet {
        use smithay::backend::allocator::Format;
        entries
            .iter()
            .map(|&(code, modifier)| Format { code, modifier })
            .collect()
    }

    #[test]
    fn hybrid_amd_primary_nvidia_secondary_selects_linear_dma_copy() {
        let amd_render = format_set(&[
            (Fourcc::Argb8888, Modifier::Linear),
            (Fourcc::Argb8888, Modifier::Unrecognized(0x0200000000000001)),
            (Fourcc::Xrgb8888, Modifier::Linear),
            (Fourcc::Argb8888, Modifier::Invalid),
        ]);
        // NVIDIA reports Linear as external_only=true, so Linear is in texture/import formats
        // (even though absent from render formats).
        let nvidia_import = format_set(&[
            (Fourcc::Argb8888, Modifier::Linear),
            (Fourcc::Xrgb8888, Modifier::Linear),
            (Fourcc::Argb8888, Modifier::Unrecognized(0x0300000000000010)),
            (Fourcc::Argb8888, Modifier::Invalid),
        ]);
        let candidates = select_dma_candidates(
            "amdgpu",
            "nvidia-drm",
            MultiGpuCopyMode::Auto,
            &amd_render,
            &nvidia_import,
        )
        .expect("AMD -> NVIDIA should find shared Linear formats");
        assert_eq!(
            candidates,
            vec![
                (Fourcc::Argb8888, vec![Modifier::Linear]),
                (Fourcc::Xrgb8888, vec![Modifier::Linear]),
            ]
        );
    }

    #[test]
    fn hybrid_nvidia_primary_amd_secondary_avoids_amdgpu_foreign_vram_cs() {
        // Even if a future NVIDIA driver advertised Linear in render_formats, Auto mode
        // must still choose CPU copy to avoid amdgpu CS -ENOMEM (-12) SIGABRT.
        let nvidia_render_with_linear = format_set(&[
            (Fourcc::Argb8888, Modifier::Linear),
            (Fourcc::Argb8888, Modifier::Unrecognized(0x0300000000000010)),
            (Fourcc::Argb8888, Modifier::Invalid),
        ]);
        let amd_import = format_set(&[
            (Fourcc::Argb8888, Modifier::Linear),
            (Fourcc::Argb8888, Modifier::Invalid),
        ]);
        assert!(select_dma_candidates(
            "nvidia-drm",
            "amdgpu",
            MultiGpuCopyMode::Auto,
            &nvidia_render_with_linear,
            &amd_import,
        )
        .is_err());

        // Real NVIDIA driver: Linear is external_only (absent from render_formats), so even
        // explicit Dma override cleanly falls back when there is no common explicit modifier.
        let nvidia_render_real = format_set(&[
            (Fourcc::Argb8888, Modifier::Unrecognized(0x0300000000000010)),
            (Fourcc::Argb8888, Modifier::Invalid),
        ]);
        assert!(select_dma_candidates(
            "nvidia-drm",
            "amdgpu",
            MultiGpuCopyMode::Dma,
            &nvidia_render_real,
            &amd_import,
        )
        .is_err());
    }

    #[test]
    fn cpu_copy_override_and_same_vendor_modifier_order() {
        let tiled = Modifier::Unrecognized(0x0200000000000001);
        let amd = format_set(&[
            (Fourcc::Argb8888, tiled),
            (Fourcc::Argb8888, Modifier::Linear),
            (Fourcc::Argb8888, Modifier::Invalid),
        ]);
        assert!(select_dma_candidates("amdgpu", "amdgpu", MultiGpuCopyMode::Cpu, &amd, &amd).is_err());
        let same = select_dma_candidates("amdgpu", "amdgpu", MultiGpuCopyMode::Auto, &amd, &amd).unwrap();
        assert_eq!(
            same,
            vec![
                (Fourcc::Argb8888, vec![Modifier::Linear]),
                (Fourcc::Argb8888, vec![tiled]),
            ]
        );
        // Cross-vendor never passes tiled vendor modifiers
        let intel = format_set(&[(Fourcc::Argb8888, tiled), (Fourcc::Argb8888, Modifier::Invalid)]);
        assert!(select_dma_candidates("amdgpu", "i915", MultiGpuCopyMode::Auto, &amd, &intel).is_err());
    }

    #[test]
    fn cpu_copy_rects_clamp_merge_and_bound_localized_updates() {
        let bounds = Size::<i32, BufferCoord>::from((1920, 1080));
        // Out-of-bounds and empty rects are dropped/clamped, touching/overlapping merge.
        let rects = merge_copy_rects(
            &[
                Rectangle::new((-10, -5).into(), (30, 25).into()),
                Rectangle::new((20, 0).into(), (15, 20).into()),
                Rectangle::new((2000, 1200).into(), (10, 10).into()),
                Rectangle::new((100, 100).into(), (0, 50).into()),
            ],
            bounds,
        );
        assert_eq!(rects, vec![Rectangle::new((0, 0).into(), (35, 20).into())]);

        // More than MAX_CPU_COPY_RECTS disjoint rects collapse to their tight bounding box,
        // not the full 1920x1080 screen.
        let many: Vec<_> = (0..6)
            .map(|i| Rectangle::new((100 + i * 30, 200).into(), (10, 10).into()))
            .collect();
        let collapsed = merge_copy_rects(&many, bounds);
        assert_eq!(collapsed, vec![Rectangle::new((100, 200).into(), (160, 10).into())]);
    }

    #[test]
    fn dma_probe_pixel_check_catches_black_swapped_rb_and_flipped_y() {
        let (w, h) = (64usize, 64usize);
        let make_img = |top: [u8; 4], bot: [u8; 4]| {
            let mut buf = vec![0u8; w * h * 4];
            for y in 0..h {
                let c = if y < h / 2 { top } else { bot };
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    buf[i..i + 4].copy_from_slice(&c);
                }
            }
            buf
        };
        let good = make_img([64, 128, 191, 255], [191, 64, 128, 255]);
        assert!(probe_pixels_match(&good, w, h, true).is_ok());

        // Black / broken EGLImage fails
        let black = make_img([0, 0, 0, 255], [0, 0, 0, 255]);
        assert!(probe_pixels_match(&black, w, h, true).is_err());

        // Swapped R/B fails
        let swapped_rb = make_img([191, 128, 64, 255], [128, 64, 191, 255]);
        assert!(probe_pixels_match(&swapped_rb, w, h, true).is_err());

        // Upside-down Y-inversion fails
        let flipped_y = make_img([191, 64, 128, 255], [64, 128, 191, 255]);
        assert!(probe_pixels_match(&flipped_y, w, h, true).is_err());

        // XRGB with zero alpha from external sampler is tolerated when check_alpha=false
        let xrgb_zero_alpha = make_img([64, 128, 191, 0], [191, 64, 128, 0]);
        assert!(probe_pixels_match(&xrgb_zero_alpha, w, h, false).is_ok());
        assert!(probe_pixels_match(&xrgb_zero_alpha, w, h, true).is_err());
    }

    #[test]
    fn headless_two_context_dma_probe_and_cpu_patch_roundtrip() {
        use std::fs::OpenOptions;
        let Some(node_path) = std::fs::read_dir("/dev/dri")
            .ok()
            .and_then(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.path())
                    .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("renderD")))
            })
        else {
            return;
        };
        let open_ctx = || -> Option<(GbmDevice<DrmDeviceFd>, GlesRenderer)> {
            let file = OpenOptions::new().read(true).write(true).open(&node_path).ok()?;
            let fd = DrmDeviceFd::new(DeviceFd::from(std::os::fd::OwnedFd::from(file)));
            let gbm = GbmDevice::new(fd).ok()?;
            let egl = unsafe { EGLDisplay::new(gbm.clone()).ok()? };
            let ctx = EGLContext::new(&egl).ok()?;
            let renderer = unsafe { GlesRenderer::new(ctx).ok()? };
            Some((gbm, renderer))
        };
        let Some((gbm1, mut primary)) = open_ctx() else { return };
        let Some((_gbm2, mut secondary)) = open_ctx() else { return };

        // 1. Real copy-strategy negotiation + DMA-BUF probe across two separate EGL/GLES contexts.
        //    GPUs with explicit Linear modifiers (e.g. RDNA/Rembrandt/Intel) must pass the full
        //    DMA-BUF pixel+orientation probe; GPUs without explicit modifiers (e.g. Polaris GFX8)
        //    must cleanly select CpuCopy without crashing.
        let has_linear = primary
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .any(|f| f.code == Fourcc::Argb8888 && f.modifier == Modifier::Linear);
        let (strategy, detail) = choose_copy_strategy(
            "amdgpu",
            "amdgpu",
            MultiGpuCopyMode::Auto,
            &mut primary,
            &gbm1,
            &mut secondary,
        );
        if has_linear {
            assert!(
                matches!(strategy, CopyStrategy::DmaCopy { .. }),
                "expected DmaCopy when GPU supports explicit Linear modifier, got {strategy:?} ({detail})"
            );
        } else {
            assert_eq!(strategy, CopyStrategy::CpuCopy);
        }

        // 2. Real CpuCopy full-frame + localized damage-rect roundtrip
        let (w, h) = (64i32, 64i32);
        let buf_size = Size::<i32, BufferCoord>::from((w, h));
        let phys_size = Size::<i32, Physical>::from((w, h));
        let mut bridge = SecondaryBridge::new_cpu((w, h), &mut primary, &mut secondary)
            .expect("create CPU bridge");
        let BridgeBacking::Cpu { offscreen, sec_texture } = &mut bridge.backing else {
            unreachable!()
        };

        // Frame 1: top/bottom two-color full clear
        let top = Rectangle::new((0, 0).into(), (w, h / 2).into());
        let bot = Rectangle::new((0, h / 2).into(), (w, h / 2).into());
        let full_mapping = {
            let mut fb = primary.bind(offscreen).unwrap();
            let mut frame = primary.render(&mut fb, phys_size, Transform::Normal).unwrap();
            frame.clear(Color32F::new(0.25, 0.50, 0.75, 1.0), &[top]).unwrap();
            frame.clear(Color32F::new(0.75, 0.25, 0.50, 1.0), &[bot]).unwrap();
            let _ = frame.finish().unwrap();
            primary.copy_framebuffer(&fb, Rectangle::from_size(buf_size), Fourcc::Abgr8888).unwrap()
        };
        upload_cpu_patches(
            &mut primary,
            &mut secondary,
            sec_texture,
            phys_size,
            vec![(full_mapping, Rectangle::from_size(buf_size))],
        )
        .unwrap();
        let read_all = |sec: &mut GlesRenderer, tex: &GlesTexture| -> Vec<u8> {
            let map = sec.copy_texture(tex, Rectangle::from_size(buf_size), Fourcc::Abgr8888).unwrap();
            sec.map_texture(&map).unwrap().to_vec()
        };
        let px1 = read_all(&mut secondary, sec_texture);
        probe_pixels_match(&px1, w as usize, h as usize, true)
            .expect("CpuCopy full-frame pixel/orientation check failed");

        // Frame 2: localized 16x16 green patch at (10, 40) only — verify partial rect transfer!
        let patch_phys = Rectangle::new((10, 40).into(), (16, 16).into());
        let patch_buf = Rectangle::<i32, BufferCoord>::new((10, 40).into(), (16, 16).into());
        let patch_mapping = {
            let mut fb = primary.bind(offscreen).unwrap();
            let mut frame = primary.render(&mut fb, phys_size, Transform::Normal).unwrap();
            frame.clear(Color32F::new(0.10, 0.90, 0.20, 1.0), &[patch_phys]).unwrap();
            let _ = frame.finish().unwrap();
            primary.copy_framebuffer(&fb, patch_buf, Fourcc::Abgr8888).unwrap()
        };
        upload_cpu_patches(
            &mut primary,
            &mut secondary,
            sec_texture,
            phys_size,
            vec![(patch_mapping, patch_buf)],
        )
        .unwrap();
        let px2 = read_all(&mut secondary, sec_texture);
        let at = |x: usize, y: usize| -> [u8; 4] {
            let i = (y * w as usize + x) * 4;
            [px2[i], px2[i + 1], px2[i + 2], px2[i + 3]]
        };
        let inside = at(18, 48);
        assert!(
            inside[0].abs_diff(26) <= 6
                && inside[1].abs_diff(230) <= 6
                && inside[2].abs_diff(51) <= 6
                && inside[3] >= 250,
            "localized patch pixel mismatch at (18, 48): {inside:?}"
        );
        // Untouched top and bottom regions outside the patch must stay intact
        let untouched_top = at(32, 16);
        assert!(
            untouched_top[0].abs_diff(64) <= 6
                && untouched_top[1].abs_diff(128) <= 6
                && untouched_top[2].abs_diff(191) <= 6,
            "top region corrupted outside patch: {untouched_top:?}"
        );
        let untouched_bot = at(50, 50);
        assert!(
            untouched_bot[0].abs_diff(191) <= 6
                && untouched_bot[1].abs_diff(64) <= 6
                && untouched_bot[2].abs_diff(128) <= 6,
            "bottom region corrupted outside patch: {untouched_bot:?}"
        );
    }
}

/// FEATHER_RENDER_DEADLINE=0 composes right after each vblank, as before
fn deadline_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !matches!(std::env::var("FEATHER_RENDER_DEADLINE").as_deref(), Ok("0" | "off" | "false")))
}

/// When the next frame of `h`, which just flipped, should be composed: ahead of the next
/// vblank by twice the recent worst frame cost, at least 4 ms and a quarter of the refresh.
/// Frames composed right after a vblank showed client commits that arrived a moment later
/// only one refresh after. VRR has no fixed vblank.
fn render_deadline(h: &Head, frame_cost: std::time::Duration) -> Option<std::time::Instant> {
    if !deadline_enabled() || h.vrr_enabled {
        return None;
    }
    deadline_wait(crate::render::refresh_period(h.refresh)?, frame_cost).map(|wait| std::time::Instant::now() + wait)
}

fn deadline_wait(period: std::time::Duration, frame_cost: std::time::Duration) -> Option<std::time::Duration> {
    use std::time::Duration;
    let margin = (frame_cost * 2 + Duration::from_millis(1)).max(Duration::from_millis(4));
    let wait = period.saturating_sub(margin).min(period / 2 + period / 4);
    (wait > Duration::from_millis(1)).then_some(wait)
}

impl FeatherCompositor {
    pub fn on_drm_vblank(&mut self, device: u64, crtc: crtc::Handle, meta: Option<smithay::backend::drm::DrmEventMetadata>) {
        use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind;
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            if let Some(h) = d.heads.iter_mut().find(|h| h.device == device && h.crtc == crtc && h.pending) {
                let success = match h.output.frame_submitted() {
                    Ok(feedback) => {
                        if let Some(mut feedback) = feedback.flatten() {
                            // The kernel's flip timestamp; without one, now (still after the flip)
                            let (time, seq, flags) = match meta {
                                Some(smithay::backend::drm::DrmEventMetadata { time: smithay::backend::drm::DrmEventTime::Monotonic(t), sequence }) => {
                                    (t.into(), sequence as u64, Kind::Vsync | Kind::HwClock | Kind::HwCompletion)
                                }
                                _ => (smithay::utils::Clock::<smithay::utils::Monotonic>::new().now(), 0, Kind::Vsync | Kind::HwCompletion),
                            };
                            let refresh = match crate::render::refresh_period(h.refresh) {
                                Some(p) if h.vrr_enabled => smithay::wayland::presentation::Refresh::variable(p),
                                Some(p) => smithay::wayland::presentation::Refresh::fixed(p),
                                None => smithay::wayland::presentation::Refresh::Unknown,
                            };
                            feedback.presented(time, refresh, seq, flags);
                        }
                        h.render_at = render_deadline(h, self.frame_cost);
                        true
                    }
                    Err(e) => {
                        tracing::warn!("DRM {}: {e:?}", h.name);
                        self.needs_redraw = true;
                        false
                    }
                };
                h.pending = false;
                h.protected = success && h.queued_protected;
            }
            self.frame_pending = d.heads.iter().any(|h| h.pending);
            if self.lock.active && !d.heads.is_empty() && d.heads.iter().all(|h| h.protected) {
                self.lock.protected_frame = true;
                self.lock_presented();
            }
        }
    }
    pub fn flush_hotplug(&mut self) {
        let specs = if let crate::backend::Backend::Drm(d) = &mut self.backend {
            if !d.active || !d.hotplug_pending {
                return;
            }
            d.hotplug_pending = false;
            d.apply_gpu_events();
            if let Err(e) = d.rescan(self.display_preference.as_ref()) {
                tracing::warn!("DRM hotplug: {e}");
                return;
            }
            self.frame_pending = d.heads.iter().any(|h| h.pending);
            d.specs()
        } else {
            return;
        };
        self.sync_monitors(specs);
        self.apply_night_shift();
    }

    pub fn on_session_pause(&mut self) {
        tracing::info!("Сессия приостановлена (переключение VT)");
        self.release_all_input();
        self.lock_sleep_barrier();
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            d.libinput.suspend();
            if let Err(e) = d.clear_all() {
                tracing::warn!("Не удалось погасить вывод перед сменой VT: {e}");
            }
            d.manager.pause();
            for g in &mut d.extra {
                g.manager.pause();
            }
            d.active = false;
        }
    }

    pub fn on_session_activate(&mut self) {
        tracing::info!("Сессия активна");
        self.power.sleeping = false;
        self.lock_session();
        self.wake_screen();
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            if d.libinput.resume().is_err() {
                tracing::warn!("libinput: не удалось возобновить");
            }
            // Disable old scanout before any new frame: never restore an unlocked framebuffer.
            if let Err(err) = d.manager.activate(true) {
                tracing::warn!("DRM: не удалось активировать: {err}");
                return;
            }
            for g in &mut d.extra {
                if let Err(err) = g.manager.activate(true) {
                    tracing::warn!("DRM {}: не удалось активировать: {err}", g.path.display());
                }
            }
            for h in &mut d.heads {
                h.output.reset_buffers();
                h.reset_bridge();
                h.pending = false;
                h.protected = false;
            }
            d.hotplug_pending = true;
            d.active = true;
        }
        // Another DRM master may have reset the gamma table while we were away
        self.apply_night_shift();
        self.frame_pending = false;
        self.needs_redraw = true;
        self.activity();
        self.rearm_idle();
    }

    pub fn switch_vt(&mut self, vt: i32) {
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            if let Err(err) = d.session.change_vt(vt) {
                tracing::warn!("Не удалось переключиться на VT {vt}: {err}");
            }
        }
    }
}
