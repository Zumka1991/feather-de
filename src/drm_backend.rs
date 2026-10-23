//! Direct DRM/KMS backend: Feather drives the GPU and input devices itself, without a host
//! compositor in between (one composition step and roughly one frame of latency less than
//! running nested in labwc).
//!
//! Scope of this first version: the primary GPU, the first connected monitor in its preferred
//! mode, libinput for all input devices, VT switching and session pause/resume.

use std::path::PathBuf;

use smithay::{
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
        },
        egl::{EGLContext, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{gles::GlesRenderer, ImportDma},
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{all_gpus, primary_gpu},
    },
    output::OutputModeSource,
    reexports::{
        calloop::LoopHandle,
        drm::control::{connector, crtc, Device as ControlDevice, ModeTypeFlags},
        input::Libinput,
        rustix::fs::OFlags,
    },
    utils::{DeviceFd, Transform},
};

use crate::{render::FeatherElement, FeatherCompositor};

pub type GbmDrmOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;
pub type GbmDrmOutputManager =
    DrmOutputManager<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// 8-bit formats only: universally supported for scan-out
const COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Xrgb8888];

pub struct DrmBackend {
    pub session: LibSeatSession,
    pub renderer: GlesRenderer,
    pub manager: GbmDrmOutputManager,
    pub output: GbmDrmOutput,
    pub libinput: Libinput,
    pub render_node: Option<DrmNode>,
    /// Physical size of the mode in use
    pub size: (i32, i32),
    /// Refresh rate in mHz
    pub refresh: i32,
    /// Physical size of the panel in millimetres (for wl_output)
    pub physical_mm: (i32, i32),
    pub connector_name: String,
    /// Session is active (not switched away to another VT)
    pub active: bool,
}

fn pick_gpu(seat: &str) -> Option<PathBuf> {
    primary_gpu(seat)
        .ok()
        .flatten()
        .or_else(|| all_gpus(seat).ok().and_then(|g| g.into_iter().next()))
}

/// First connected connector with a CRTC it can drive, and its preferred mode
fn pick_connector(drm: &DrmDevice) -> Option<(connector::Info, crtc::Handle, smithay::reexports::drm::control::Mode)> {
    let res = drm.resource_handles().ok()?;
    for &conn in res.connectors() {
        let Ok(info) = drm.get_connector(conn, true) else { continue };
        if info.state() != connector::State::Connected || info.modes().is_empty() {
            continue;
        }
        let mode = info
            .modes()
            .iter()
            .copied()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .unwrap_or(info.modes()[0]);
        let crtc = info
            .encoders()
            .iter()
            .filter_map(|e| drm.get_encoder(*e).ok())
            .flat_map(|enc| res.filter_crtcs(enc.possible_crtcs()))
            .next();
        if let Some(crtc) = crtc {
            return Some((info, crtc, mode));
        }
    }
    None
}

/// Opens the GPU, the first monitor and libinput, and registers their event sources
pub fn init(handle: &LoopHandle<'static, FeatherCompositor>) -> Result<DrmBackend, Box<dyn std::error::Error>> {
    let (mut session, notifier) = LibSeatSession::new().map_err(|e| format!("libseat: {e}"))?;
    let seat = session.seat();

    let gpu_path = pick_gpu(&seat).ok_or("видеокарта не найдена")?;
    tracing::info!("DRM: устройство {}", gpu_path.display());
    let fd = session
        .open(
            &gpu_path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|e| format!("не удалось открыть {}: {e}", gpu_path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd.clone())?;

    let egl = unsafe { EGLDisplay::new(gbm.clone())? };
    let context = EGLContext::new(&egl)?;
    let mut renderer = unsafe { GlesRenderer::new(context)? };

    let render_node = DrmNode::from_file(&fd)
        .ok()
        .and_then(|n| n.node_with_type(NodeType::Render).and_then(Result::ok));

    let (conn_info, crtc, mode) = pick_connector(&drm).ok_or("нет подключённого монитора")?;
    let connector_name = format!("{}-{}", conn_info.interface().as_str(), conn_info.interface_id());
    let (w, h) = (mode.size().0 as i32, mode.size().1 as i32);
    let refresh = (mode.vrefresh() as i32) * 1000;
    let physical_mm = conn_info
        .size()
        .map(|(a, b)| (a as i32, b as i32))
        .unwrap_or((0, 0));
    tracing::info!("DRM: {connector_name} {w}x{h}@{}Hz", mode.vrefresh());

    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), render_node);
    let render_formats = renderer.egl_context().dmabuf_render_formats().clone();
    let mut manager = GbmDrmOutputManager::new(
        drm,
        allocator,
        exporter,
        Some(gbm),
        COLOR_FORMATS,
        render_formats,
    );

    // Our scene is built in physical pixels at scale 1.0 (see render.rs)
    let mode_source = OutputModeSource::Static {
        size: (w, h).into(),
        scale: 1.0.into(),
        transform: Transform::Normal,
    };
    let output = manager
        .initialize_output::<_, FeatherElement>(
            crtc,
            mode,
            &[conn_info.handle()],
            mode_source,
            None,
            &mut renderer,
            &DrmOutputRenderElements::default(),
        )
        .map_err(|e| format!("не удалось инициализировать вывод: {e:?}"))?;

    // VBlank → previous frame is on screen, next may be rendered
    handle.insert_source(drm_notifier, |event, _, state| match event {
        DrmEvent::VBlank(_) => state.on_drm_vblank(),
        DrmEvent::Error(err) => tracing::warn!("DRM: {err}"),
    })?;

    // Input
    let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
    libinput
        .udev_assign_seat(&seat)
        .map_err(|_| "libinput: не удалось назначить seat")?;
    handle.insert_source(LibinputInputBackend::new(libinput.clone()), |mut event, _, state| {
        if let smithay::backend::input::InputEvent::DeviceAdded { device } = &mut event {
            // Touchpads behave like on a Mac: tap to click, natural scrolling
            if device.config_tap_finger_count() > 0 {
                let _ = device.config_tap_set_enabled(true);
                let _ = device.config_scroll_set_natural_scroll_enabled(true);
            }
        }
        state.handle_input(event);
    })?;

    // VT switches pause/resume the whole session
    handle.insert_source(notifier, |event, _, state| match event {
        SessionEvent::PauseSession => state.on_session_pause(),
        SessionEvent::ActivateSession => state.on_session_activate(),
    })?;

    // dmabuf formats are queried from the renderer later; make sure it can import at all
    let _ = renderer.dmabuf_formats();

    Ok(DrmBackend {
        session,
        renderer,
        manager,
        output,
        libinput,
        render_node,
        size: (w, h),
        refresh,
        physical_mm,
        connector_name,
        active: true,
    })
}

impl FeatherCompositor {
    pub fn on_drm_vblank(&mut self) {
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            if let Err(err) = d.output.frame_submitted() {
                tracing::warn!("DRM frame_submitted: {err:?}");
            }
        }
        self.on_host_frame();
    }

    pub fn on_session_pause(&mut self) {
        tracing::info!("Сессия приостановлена (переключение VT)");
        self.release_all_input();
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            d.libinput.suspend();
            d.manager.pause();
            d.active = false;
        }
    }

    pub fn on_session_activate(&mut self) {
        tracing::info!("Сессия активна");
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            if d.libinput.resume().is_err() {
                tracing::warn!("libinput: не удалось возобновить");
            }
            if let Err(err) = d.manager.activate(false) {
                tracing::warn!("DRM: не удалось активировать: {err}");
            }
            d.output.reset_buffers();
            d.active = true;
        }
        self.frame_pending = false;
        self.needs_redraw = true;
    }

    pub fn switch_vt(&mut self, vt: i32) {
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            if let Err(err) = d.session.change_vt(vt) {
                tracing::warn!("Не удалось переключиться на VT {vt}: {err}");
            }
        }
    }
}
