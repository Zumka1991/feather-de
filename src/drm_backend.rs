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
pub type GbmDrmOutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;

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
    pub modes: Vec<smithay::reexports::drm::control::Mode>,
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
fn pick_connector(
    drm: &DrmDevice,
) -> Option<(
    connector::Info,
    crtc::Handle,
    smithay::reexports::drm::control::Mode,
)> {
    let res = drm.resource_handles().ok()?;
    for &conn in res.connectors() {
        let Ok(info) = drm.get_connector(conn, true) else {
            continue;
        };
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
pub fn init(
    handle: &LoopHandle<'static, FeatherCompositor>,
    saved: Option<&crate::settings::Display>,
) -> Result<DrmBackend, Box<dyn std::error::Error>> {
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

    let (conn_info, crtc, preferred) = pick_connector(&drm).ok_or("нет подключённого монитора")?;
    let connector_name = format!(
        "{}-{}",
        conn_info.interface().as_str(),
        conn_info.interface_id()
    );
    let modes = conn_info.modes().to_vec();
    let mode = saved
        .filter(|s| s.name == connector_name)
        .and_then(|s| modes.iter().find(|m| mode_info(**m) == s.mode))
        .copied()
        .unwrap_or(preferred);
    let (mut w, mut h) = (mode.size().0 as i32, mode.size().1 as i32);
    let mut refresh = mode_info(mode).refresh;
    let physical_mm = conn_info
        .size()
        .map(|(a, b)| (a as i32, b as i32))
        .unwrap_or((0, 0));
    tracing::info!("DRM: {connector_name} {w}x{h}@{}Hz", mode.vrefresh());

    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
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
    let output = match manager.initialize_output::<_, FeatherElement>(
        crtc,
        mode,
        &[conn_info.handle()],
        mode_source,
        None,
        &mut renderer,
        &DrmOutputRenderElements::default(),
    ) {
        Ok(output) => output,
        Err(first) if mode != preferred => {
            tracing::warn!("Сохранённый режим недоступен: {first:?}; пробуем предпочтительный");
            w = preferred.size().0 as i32;
            h = preferred.size().1 as i32;
            refresh = mode_info(preferred).refresh;
            manager
                .initialize_output::<_, FeatherElement>(
                    crtc,
                    preferred,
                    &[conn_info.handle()],
                    OutputModeSource::Static {
                        size: (w, h).into(),
                        scale: 1.0.into(),
                        transform: Transform::Normal,
                    },
                    None,
                    &mut renderer,
                    &DrmOutputRenderElements::default(),
                )
                .map_err(|e| format!("не удалось инициализировать предпочтительный вывод: {e:?}"))?
        }
        Err(e) => return Err(format!("не удалось инициализировать вывод: {e:?}").into()),
    };

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
    handle.insert_source(
        LibinputInputBackend::new(libinput.clone()),
        |mut event, _, state| {
            match &mut event {
                smithay::backend::input::InputEvent::DeviceAdded { device } => {
                    state.configure_input_device(device);
                    state.input_devices.push(device.clone());
                }
                smithay::backend::input::InputEvent::DeviceRemoved { device } => {
                    state.input_devices.retain(|d| d != device);
                }
                _ => {}
            }
            state.handle_input(event);
        },
    )?;

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
        modes,
        active: true,
    })
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
        self.lock_sleep_barrier();
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            d.libinput.suspend();
            if let Err(e)=d.output.with_compositor(|c|c.clear()) {tracing::warn!("Не удалось погасить вывод перед сменой VT: {e}");}
            d.manager.pause();
            d.active = false;
        }
    }

    pub fn on_session_activate(&mut self) {
        tracing::info!("Сессия активна");
        self.power.sleeping=false;
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
            d.output.reset_buffers();
            d.active = true;
        }
        self.frame_pending = false;
        self.needs_redraw = true;
        self.activity();self.rearm_idle();
    }

    pub fn switch_vt(&mut self, vt: i32) {
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            if let Err(err) = d.session.change_vt(vt) {
                tracing::warn!("Не удалось переключиться на VT {vt}: {err}");
            }
        }
    }
}
