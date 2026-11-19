//! Direct DRM/KMS backend: one scanout and damage history per connected connector.
//! The primary GPU renders every output; hotplug is driven by udev, never polling.
//! Connectors of other GPUs (hybrid laptops, docks) are scanned out by their own device:
//! the frame is still composed by the primary renderer into a linear buffer both can use.
use crate::{render::OutputElement, FeatherCompositor};
use smithay::{
    backend::{
        allocator::{
            dmabuf::AsDmabuf,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Allocator, Fourcc, Modifier,
        },
        drm::{
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
        },
        egl::{EGLContext, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{gles::GlesRenderer, Bind, ImportDma},
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{all_gpus, primary_gpu, UdevBackend, UdevEvent},
    },
    output::OutputModeSource,
    reexports::{
        calloop::{LoopHandle, RegistrationToken},
        drm::control::{connector, crtc, Device as ControlDevice, ModeTypeFlags},
        input::Libinput,
        rustix::fs::OFlags,
    },
    utils::{DeviceFd, Transform},
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
const OPEN_FLAGS: OFlags = OFlags::RDWR.union(OFlags::CLOEXEC).union(OFlags::NOCTTY).union(OFlags::NONBLOCK);
/// A video card other than the rendering one: only its connectors are used
pub struct Gpu {
    pub manager: GbmDrmOutputManager,
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
    pub extra: Vec<Gpu>,
    /// Cards that appeared (`Some(path)`) or went away since the last `flush_hotplug`
    pub gpu_events: Vec<(u64, Option<PathBuf>)>,
    pub device: u64,
    gbm: GbmDevice<DrmDeviceFd>,
    handle: LoopHandle<'static, FeatherCompositor>,
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
    /// Every card is scanned again; a card that fails keeps the others working
    pub fn rescan(
        &mut self,
        saved: Option<&crate::settings::Display>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let primary = scan(&mut self.manager, &mut self.renderer, &mut self.heads, self.device, &self.fd, saved);
        for g in &mut self.extra {
            let fd = g.manager.device().device_fd().clone();
            if let Err(e) = scan(&mut g.manager, &mut self.renderer, &mut self.heads, g.device, &fd, saved) {
                tracing::warn!("DRM {}: {e}", g.path.display());
            }
        }
        primary
    }
    /// Opens another card for its connectors. The primary renderer draws straight into
    /// that card's linear buffers when it can import them; otherwise the frame is drawn
    /// into a linear buffer of the primary card that the other one imports for scanout.
    fn open_secondary(&mut self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let fd = self.session.open(path, OPEN_FLAGS).map_err(|e| format!("{}: {e}", path.display()))?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let device = DrmNode::from_file(&fd)?.dev_id() as u64;
        if device == self.device || self.extra.iter().any(|g| g.device == device) {
            return Ok(());
        }
        tracing::info!("DRM: probing secondary card {}", path.display());
        let (drm, notifier) = DrmDevice::new(fd.clone(), true)?;
        let gbm = GbmDevice::new(fd.clone())?;
        let linear: Vec<_> = self
            .renderer
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .filter(|f| f.modifier == Modifier::Linear)
            .copied()
            .collect();
        let mut own = GbmAllocator::new(gbm.clone(), GbmBufferFlags::SCANOUT);
        let direct = own
            .create_buffer(256, 256, COLOR_FORMATS[0], &[Modifier::Linear])
            .ok()
            .and_then(|b| b.export().ok())
            .is_some_and(|mut d| self.renderer.bind(&mut d).is_ok());
        let allocator = if direct { own } else { GbmAllocator::new(self.gbm.clone(), GbmBufferFlags::RENDERING) };
        // No cursor plane and no client scanout here: both need buffers of this very card
        let manager = GbmDrmOutputManager::new(
            drm,
            allocator,
            GbmFramebufferExporter::new(gbm, None),
            None,
            COLOR_FORMATS,
            linear,
        );
        let token = self.handle.insert_source(notifier, move |event, metadata, state| match event {
            DrmEvent::VBlank(crtc) => state.on_drm_vblank(device, crtc, metadata.take()),
            DrmEvent::Error(e) => tracing::warn!("DRM: {e}"),
        })?;
        tracing::info!(
            "DRM: вторая видеокарта {} ({})",
            path.display(),
            if direct { "отрисовка в её буферы" } else { "вывод буферов основной карты" }
        );
        self.extra.push(Gpu { manager, device, path: path.to_path_buf(), token });
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
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd.clone())?;
    let primary_gbm = gbm.clone();
    let egl = unsafe { EGLDisplay::new(gbm.clone())? };
    let context = EGLContext::new(&egl)?;
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
        session,
        renderer,
        manager,
        extra: Vec::new(),
        gpu_events: Vec::new(),
        device,
        gbm: primary_gbm,
        handle: handle.clone(),
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
