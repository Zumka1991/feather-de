//! Direct DRM/KMS backend: one scanout and damage history per connected connector.
//! The primary GPU is shared by all outputs; hotplug is driven by udev, never polling.
use crate::{render::OutputElement, FeatherCompositor};
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
        udev::{all_gpus, primary_gpu, UdevBackend, UdevEvent},
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
use std::path::PathBuf;
pub type GbmDrmOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;
pub type GbmDrmOutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;
const COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Xrgb8888];
pub struct Head {
    pub output: GbmDrmOutput,
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
}
pub struct DrmBackend {
    pub session: LibSeatSession,
    pub renderer: GlesRenderer,
    pub manager: GbmDrmOutputManager,
    pub heads: Vec<Head>,
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
fn pick_gpu(seat: &str) -> Option<PathBuf> {
    primary_gpu(seat)
        .ok()
        .flatten()
        .or_else(|| all_gpus(seat).ok().and_then(|g| g.into_iter().next()))
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
    pub fn clear_all(&mut self) -> Result<(), String> {
        let mut error = None;
        for h in &mut self.heads {
            match h.output.with_compositor(|c| c.clear()) {
                Ok(()) => {
                    h.pending = false;
                    h.protected = true;
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
    pub fn rescan(
        &mut self,
        saved: Option<&crate::settings::Display>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let drm = self.manager.device();
        let res = drm.resource_handles()?;
        let connectors: Vec<_> = res
            .connectors()
            .iter()
            .filter_map(|c| drm.get_connector(*c, true).ok())
            .filter(|c| c.state() == connector::State::Connected && !c.modes().is_empty())
            .take(8)
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
                if let Some(existing) = self
                    .heads
                    .iter()
                    .find(|h| h.connector == c.handle())
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
        self.heads.retain_mut(|h| {
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
            if let Some(h) = self.heads.iter_mut().find(|h| h.connector == info.handle()) {
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
            let output = match create(mode, &mut self.manager, &mut self.renderer) {
                Ok(o) => o,
                Err(e) if mode != preferred => {
                    tracing::warn!("DRM {name}: сохранённый режим недоступен: {e:?}");
                    mode = preferred;
                    match create(mode, &mut self.manager, &mut self.renderer) {
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
            self.heads.push(Head {
                output,
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
            });
        }
        Ok(())
    }
}
pub fn init(
    handle: &LoopHandle<'static, FeatherCompositor>,
    saved: Option<&crate::settings::Display>,
) -> Result<DrmBackend, Box<dyn std::error::Error>> {
    let (mut session, notifier) = LibSeatSession::new().map_err(|e| format!("libseat: {e}"))?;
    let seat = session.seat();
    let path = pick_gpu(&seat).ok_or("видеокарта не найдена")?;
    let fd = session
        .open(
            &path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd.clone())?;
    let egl = unsafe { EGLDisplay::new(gbm.clone())? };
    let context = EGLContext::new(&egl)?;
    let renderer = unsafe { GlesRenderer::new(context)? };
    let node = DrmNode::from_file(&fd).ok();
    let device_id = node.map(|n| n.dev_id());
    let render_node = node.and_then(|n| n.node_with_type(NodeType::Render).and_then(Result::ok));
    let formats = renderer.egl_context().dmabuf_render_formats().clone();
    let manager = GbmDrmOutputManager::new(
        drm,
        GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        ),
        GbmFramebufferExporter::new(gbm.clone(), render_node),
        Some(gbm),
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
            state.handle_input(event);
        },
    )?;
    handle.insert_source(drm_notifier, |event, _, state| match event {
        DrmEvent::VBlank(crtc) => state.on_drm_vblank(crtc),
        DrmEvent::Error(e) => tracing::warn!("DRM: {e}"),
    })?;
    handle.insert_source(notifier, |event, _, state| match event {
        SessionEvent::PauseSession => state.on_session_pause(),
        SessionEvent::ActivateSession => state.on_session_activate(),
    })?;
    handle.insert_source(UdevBackend::new(&seat)?, move |event, _, state| {
        let id = match event {
            UdevEvent::Added { device_id, .. }
            | UdevEvent::Changed { device_id }
            | UdevEvent::Removed { device_id } => device_id,
        };
        if Some(id) == device_id {
            if let crate::backend::Backend::Drm(d) = &mut state.backend {
                d.hotplug_pending = true;
            }
        }
    })?;
    let mut backend = DrmBackend {
        session,
        renderer,
        manager,
        heads: Vec::new(),
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
}

impl FeatherCompositor {
    pub fn on_drm_vblank(&mut self, crtc: crtc::Handle) {
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            if let Some(h) = d.heads.iter_mut().find(|h| h.crtc == crtc && h.pending) {
                let success = match h.output.frame_submitted() {
                    Ok(_) => true,
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
            for h in &mut d.heads {
                h.output.reset_buffers();
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
