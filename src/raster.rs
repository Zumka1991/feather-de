//! CPU-only assets. The event loop submits bounded jobs and installs finished buffers.
//! No renderer, EGL context, or live shell state crosses the thread boundary.
use crate::{
    icons::{IconKey, IconLoader},
    theme::{Appearance, WallpaperTheme},
    FeatherCompositor,
};
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    reexports::calloop::{channel, LoopHandle},
    utils::{Rectangle, Transform},
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc, Weak,
};

pub type BackgroundKey = (WallpaperTheme, i32, i32, bool);

pub struct TileJob {
    pub ticket: Weak<()>,
    pub app: usize,
    pub icon: crate::shell::AppIcon,
    pub label: String,
    pub layout: crate::launchpad::LaunchpadLayout,
}

pub enum Job {
    Settings(crate::system_settings::FrameJob),
    Reclaim,
    CcBackdrop {
        screen: (i32, i32),
    },
    Wallpaper {
        logical: (i32, i32),
        theme: WallpaperTheme,
        image: Option<crate::wallpaper::ImageSource>,
    },
    Frosted(BackgroundKey),
    Icon(IconKey),
    Tile(TileJob),
    DesktopTile(crate::desktop::TileJob),
}

pub enum Asset {
    Settings(std::sync::Weak<()>, crate::window::SettingsFrame),
    Retry,
    Reclaimed,
    CcBackdrop(crate::shell::CcBackdrop),
    Wallpaper {
        pixels: Vec<u8>,
        size: (i32, i32),
        image: Option<crate::wallpaper::ImageSource>,
        glass: tiny_skia::Pixmap,
    },
    WallpaperError(String),
    Frosted(BackgroundKey, MemoryRenderBuffer),
    Icon(IconKey, Option<crate::icons::IconPixels>),
    Tile {
        ticket: Weak<()>,
        app: usize,
        buffer: MemoryRenderBuffer,
    },
    DesktopTile(crate::desktop::TilePixels),
}

struct Request {
    epoch: u64,
    scale: f32,
    appearance: Appearance,
    accent: crate::theme::AccentColor,
    highlight: Option<crate::theme::AccentColor>,
    text_size: crate::theme::TextSize,
    high_contrast: bool,
    reduce_transparency: bool,
    job: Job,
}

#[derive(Clone)]
pub struct RasterSender {
    requests: mpsc::SyncSender<Request>,
    epoch: Arc<AtomicU64>,
}

impl RasterSender {
    pub fn new(
        handle: &LoopHandle<'static, FeatherCompositor>,
        mut fonts: crate::text::FontSystem,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (requests, jobs) = mpsc::sync_channel::<Request>(64);
        let (finished, source) = channel::sync_channel(2);
        let epoch = Arc::new(AtomicU64::new(1));
        handle.insert_source(source, |event, _, state| {
            if let channel::Event::Msg((epoch, asset)) = event {
                let settings_frame = matches!(&asset, Asset::Settings(..));
                let wallpaper_ready = matches!(&asset, Asset::Wallpaper { .. });
                if state
                    .shell
                    .raster
                    .as_ref()
                    .is_some_and(|r| r.current(epoch))
                    && match asset {
                        Asset::Settings(ticket, frame) => state
                            .windows
                            .iter_mut()
                            .find(|w| w.app_id == "builtin.settings")
                            .is_some_and(|w| w.install_settings_frame(&ticket, frame)),
                        Asset::WallpaperError(error) => {
                            state.shell.wallpaper_image = if state.shell.wallpaper_image
                                == state.shell.applied_wallpaper_image
                            {
                                None
                            } else {
                                state.shell.applied_wallpaper_image.clone()
                            };
                            state.shell.wallpaper_dirty = true;
                            state.system_settings.error = Some(error);
                            state.mark_settings_dirty();
                            true
                        }
                        asset => state.shell.install_asset(asset),
                    }
                {
                    state.needs_redraw = true;
                    // New frosted wallpaper behind the lock panel
                    state.lock.repaint();
                    if wallpaper_ready {
                        state.shell.upload_wallpaper(state.backend.renderer());
                        state.save_settings();
                    }
                }
                if settings_frame {
                    // The temporary CPU frame and obsolete textures are gone after
                    // the next submission. Return their free pages on the worker.
                    state.shell.request_raster_reclaim();
                    state.needs_redraw = true;
                }
            }
        })?;
        let latest = epoch.clone();
        let profile = std::env::var_os("FEATHER_PROFILE").is_some();
        std::thread::Builder::new()
            .name("feather-raster".into())
            .stack_size(1024 * 1024)
            .spawn(move || {
                let mut icons = IconLoader::new();
                let mut file_icons = feather_ui::file_icons::IconCache::default();
                let mut glass_background = None;
                let mut settings_window = crate::window::ManagedWindow::new_builtin(
                    0,
                    crate::apps::BuiltinWindowKind::SystemSettings,
                    0,
                    0,
                );
                while let Ok(request) = jobs.recv() {
                    let valid = || {
                        request.epoch == latest.load(Ordering::Acquire)
                            && match &request.job {
                                Job::Tile(t) => t.ticket.strong_count() > 0,
                                Job::Settings(t) => t.ticket.strong_count() > 0,
                                _ => true,
                            }
                    };
                    if !valid() {
                        // A full queue may have rejected newer work. Wake the loop when
                        // discarding old jobs, so that no request relies on another input event.
                        if finished
                            .send((latest.load(Ordering::Acquire), Asset::Retry))
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    let start = std::time::Instant::now();
                    let kind = match &request.job {
                        Job::Settings(_) => "settings",
                        Job::Reclaim => "reclaim",
                        Job::CcBackdrop { .. } => "cc-backdrop",
                        Job::Wallpaper { .. } => "wallpaper",
                        Job::Frosted(_) => "frosted",
                        Job::Icon(_) => "icon",
                        Job::Tile(_) => "tile",
                        Job::DesktopTile(_) => "desktop-tile",
                    };
                    let asset = crate::theme::with_full_render_settings(
                        request.scale,
                        request.appearance,
                        request.accent,
                        request.highlight,
                        request.text_size,
                        request.high_contrast,
                        request.reduce_transparency,
                        || match &request.job {
                            Job::Settings(job) => Asset::Settings(
                                job.ticket.clone(),
                                settings_window.raster_settings(&mut fonts, job),
                            ),
                            Job::Reclaim => {
                                // Closing Launchpad releases a burst of pixel/SVG allocations
                                // across arenas. Return free pages, without allocator tuning or
                                // any periodic trim/wait on the compositor thread.
                                #[cfg(target_env = "gnu")]
                                unsafe {
                                    libc::malloc_trim(0);
                                }
                                Asset::Reclaimed
                            }
                            Job::Wallpaper {
                                logical: (w, h),
                                theme,
                                image,
                            } => {
                                let (pw, ph) = if image.is_some() {
                                    crate::wallpaper::image_size(
                                        crate::theme::to_phys(*w),
                                        crate::theme::to_phys(*h),
                                    )
                                } else {
                                    crate::shell::wallpaper_buffer_size(*w, *h)
                                };
                                let data = if let Some(source) = image {
                                    match crate::wallpaper::decode(source, pw, ph) {
                                        Ok(pixels) => pixels,
                                        Err(e) => return Asset::WallpaperError(e),
                                    }
                                } else {
                                    let mut pixels = vec![0; pw as usize * ph as usize * 4];
                                    crate::theme::render_wallpaper(&mut pixels, pw, ph, *theme);
                                    pixels
                                };
                                let (gw, gh) = (
                                    (crate::theme::to_phys(*w) / 8).max(8),
                                    (crate::theme::to_phys(*h) / 8).max(8),
                                );
                                let glass = if image.is_some() {
                                    crate::wallpaper::glass(
                                        &data,
                                        (pw, ph),
                                        (gw, gh),
                                        if request.appearance == Appearance::Dark {
                                            0.8
                                        } else {
                                            1.0
                                        },
                                    )
                                } else {
                                    let data = crate::theme::render_frosted_wallpaper(
                                        gw,
                                        gh,
                                        *theme,
                                        if request.appearance == Appearance::Dark {
                                            0.8
                                        } else {
                                            1.0
                                        },
                                    );
                                    tiny_skia::Pixmap::from_vec(
                                        data,
                                        tiny_skia::IntSize::from_wh(gw as u32, gh as u32).unwrap(),
                                    )
                                    .unwrap()
                                };
                                glass_background = Some((
                                    std::rc::Rc::new(glass.clone()),
                                    *w as f32 / gw as f32,
                                    *h as f32 / gh as f32,
                                ));
                                Asset::Wallpaper {
                                    pixels: data,
                                    size: (pw, ph),
                                    image: image.clone(),
                                    glass,
                                }
                            }
                            Job::CcBackdrop { screen } => {
                                crate::theme::set_glass_background(glass_background.clone());
                                let backdrop = crate::shell::MacShell::raster_cc_backdrop(*screen);
                                crate::theme::set_glass_background(None);
                                Asset::CcBackdrop(backdrop)
                            }
                            Job::Frosted(key) => {
                                let data = if let Some((glass, _, _)) = &glass_background {
                                    crate::wallpaper::glass(
                                        glass.data(),
                                        (glass.width() as i32, glass.height() as i32),
                                        (key.1, key.2),
                                        if key.3 { 0.55 } else { 0.72 },
                                    )
                                    .take()
                                } else {
                                    crate::theme::render_frosted_wallpaper(
                                        key.1,
                                        key.2,
                                        key.0,
                                        if key.3 { 0.55 } else { 0.72 },
                                    )
                                };
                                Asset::Frosted(*key, buffer(&data, key.1, key.2, true))
                            }
                            Job::Icon(key) => Asset::Icon(
                                key.clone(),
                                icons
                                    .load_app(key, &mut fonts)
                                    .map(crate::icons::IconPixels::new),
                            ),
                            Job::DesktopTile(tile) => Asset::DesktopTile(
                                crate::desktop::raster_tile(tile, &mut fonts, &mut file_icons, &mut icons),
                            ),
                            Job::Tile(tile) => Asset::Tile {
                                ticket: tile.ticket.clone(),
                                app: tile.app,
                                buffer: crate::launchpad::raster_tile(tile, &mut fonts, &mut icons),
                            },
                        },
                    );
                    if matches!(request.job, Job::Settings(_) | Job::Wallpaper { .. }) {
                        #[cfg(target_env = "gnu")]
                        unsafe {
                            libc::malloc_trim(0);
                        }
                    }
                    if profile {
                        eprintln!(
                            "[feather-raster] {kind}: {:.2} мс (worker)",
                            start.elapsed().as_secs_f64() * 1000.0
                        );
                    }
                    let update = if valid() {
                        (request.epoch, asset)
                    } else {
                        (latest.load(Ordering::Acquire), Asset::Retry)
                    };
                    if finished.send(update).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self { requests, epoch })
    }

    pub fn submit(&self, job: Job) -> bool {
        self.requests
            .try_send(Request {
                epoch: self.epoch.load(Ordering::Acquire),
                scale: crate::theme::ui_scale(),
                appearance: crate::theme::appearance(),
                accent: crate::theme::accent_color(),
                highlight: crate::theme::highlight_choice(),
                text_size: crate::theme::text_size(),
                high_contrast: crate::theme::high_contrast(),
                reduce_transparency: crate::theme::reduce_transparency(),
                job,
            })
            .is_ok()
    }
    pub fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::Release);
    }
    fn current(&self, epoch: u64) -> bool {
        epoch == self.epoch.load(Ordering::Acquire)
    }
}

fn buffer(data: &[u8], w: i32, h: i32, opaque: bool) -> MemoryRenderBuffer {
    MemoryRenderBuffer::from_slice(
        data,
        Fourcc::Abgr8888,
        (w, h),
        1,
        Transform::Normal,
        opaque.then(|| vec![Rectangle::from_size((w, h).into())]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn themed(name: &str, px: u32) -> IconKey {
        IconKey::new(
            &crate::shell::AppIcon {
                kind: crate::apps::IconKind::GenericApp,
                name: Some(name.into()),
            },
            name,
            px,
        )
    }

    #[test]
    fn settings_snapshot_is_local_and_restores_after_nested_renders() {
        let before = (crate::theme::ui_scale(), crate::theme::appearance());
        crate::theme::with_render_settings(1.25, Appearance::Dark, || {
            let child = std::thread::spawn(|| {
                crate::theme::with_render_settings(2.5, Appearance::Light, || {
                    assert_eq!(crate::theme::ui_scale(), 2.5);
                    assert_eq!(crate::theme::appearance(), Appearance::Light);
                });
            });
            child.join().unwrap();
            assert_eq!(crate::theme::ui_scale(), 1.25);
            assert_eq!(crate::theme::appearance(), Appearance::Dark);
            let _ = std::panic::catch_unwind(|| {
                crate::theme::with_render_settings(3.0, Appearance::Light, || panic!("cancel"))
            });
            assert_eq!(crate::theme::ui_scale(), 1.25);
        });
        assert_eq!(
            (crate::theme::ui_scale(), crate::theme::appearance()),
            before
        );
    }

    #[test]
    fn icon_miss_queues_once_and_full_queue_can_retry_after_progress() {
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = RasterSender {
            requests: tx,
            epoch: Arc::new(AtomicU64::new(1)),
        };
        let mut cache = crate::icons::IconCache::new();
        cache.attach(worker.clone());
        let key = themed("not-read-on-event-thread.svg", 192);
        let another = themed("another", 192);
        assert!(cache.get(&key).is_none());
        assert!(cache.get(&key).is_none());
        cache.get(&another);
        assert!(!cache.pending(&another));
        let request = rx.try_recv().unwrap();
        assert!(matches!(request.job, Job::Icon(_)));
        assert!(rx.try_recv().is_err());
        cache.get(&another);
        assert!(cache.pending(&another));
        assert!(rx.try_recv().is_ok());
        assert!(worker.current(request.epoch));
        worker.invalidate();
        assert!(!worker.current(request.epoch));
    }

    #[test]
    fn whole_pixel_icons_are_copied_from_an_exact_size_without_a_second_job() {
        let (tx, rx) = mpsc::sync_channel(4);
        let mut cache = crate::icons::IconCache::new();
        cache.attach(RasterSender {
            requests: tx,
            epoch: Arc::new(AtomicU64::new(1)),
        });
        let mut fonts = crate::text::FontSystem::new();
        let solid = |px: u32, r: u8| {
            let mut pm = tiny_skia::Pixmap::new(px, px).unwrap();
            pm.fill(tiny_skia::Color::from_rgba8(r, 0, 0, 255));
            pm
        };
        let pixel = |pm: &tiny_skia::Pixmap, x: u32, y: u32| pm.pixel(x, y).unwrap().red();
        crate::theme::with_render_settings(2.0, Appearance::Light, || {
            let icon = themed("exact", 1).icon;
            let draw = |cache: &mut crate::icons::IconCache,
                        fonts: &mut crate::text::FontSystem| {
                let mut out = tiny_skia::Pixmap::new(140, 140).unwrap();
                crate::shell::draw_any_icon(
                    &mut out.as_mut(),
                    fonts,
                    cache,
                    &icon,
                    "exact",
                    10.2,
                    10.0,
                    50.0,
                );
                out
            };
            // Before the exact image exists the cached bucket is resampled; only the
            // exact size is queued.
            cache.install(
                themed("exact", 192),
                Some(crate::icons::IconPixels::new(solid(192, 90))),
            );
            let out = draw(&mut cache, &mut fonts);
            assert!(pixel(&out, 70, 70) > 80);
            let request = rx.try_recv().unwrap();
            assert!(matches!(request.job, Job::Icon(ref k) if k.px == 100));
            assert!(rx.try_recv().is_err());
            // 10.2 logical × 2 snaps to physical column 20; the copy is unscaled.
            cache.install(
                themed("exact", 100),
                Some(crate::icons::IconPixels::new(solid(100, 200))),
            );
            let out = draw(&mut cache, &mut fonts);
            assert_eq!(
                (
                    pixel(&out, 19, 50),
                    pixel(&out, 20, 50),
                    pixel(&out, 119, 50),
                    pixel(&out, 120, 50)
                ),
                (0, 200, 200, 0)
            );
            assert!(rx.try_recv().is_err());
            // A preview left by eviction is still scaled to the requested size.
            cache.install(
                themed("exact", 100),
                Some(crate::icons::IconPixels {
                    full: None,
                    preview: solid(64, 150),
                }),
            );
            let out = draw(&mut cache, &mut fonts);
            assert!(pixel(&out, 115, 115) > 140);
            // Another surface size (Spotlight 30 → 60 px) scales a cached size meanwhile.
            let mut out = tiny_skia::Pixmap::new(140, 140).unwrap();
            crate::shell::draw_any_icon(
                &mut out.as_mut(),
                &mut fonts,
                &mut cache,
                &icon,
                "exact",
                0.0,
                0.0,
                30.0,
            );
            assert!(pixel(&out, 30, 30) > 80);
            assert!(matches!(rx.try_recv().unwrap().job, Job::Icon(ref k) if k.px == 60));
            // A magnified Dock icon that lands on a whole pixel resamples the cached bucket
            // and queues nothing for its transient size.
            crate::shell::draw_resampled_icon(
                &mut out.as_mut(),
                &mut cache,
                &icon,
                "exact",
                0.0,
                0.0,
                60.0,
            );
            assert!(pixel(&out, 60, 60) > 80);
            assert!(rx.try_recv().is_err() && !cache.pending(&themed("exact", 120)));
        });
        assert_eq!(crate::icons::exact_icon_px(62.5), None);
        assert_eq!(crate::icons::exact_icon_px(75.0), Some(75));
        assert_eq!(crate::icons::exact_icon_px(400.0), None);
    }

    #[test]
    fn icon_cache_eviction_keeps_a_preview_instead_of_a_decode_loop() {
        let mut cache = crate::icons::IconCache::new();
        for i in 0..14 {
            cache.install(
                themed(&format!("icon{i}"), 320),
                tiny_skia::Pixmap::new(320, 320).map(crate::icons::IconPixels::new),
            );
        }
        assert_eq!(cache.get(&themed("icon0", 320)).unwrap().width(), 64);
        assert!(!cache.pending(&themed("icon0", 320)));
        assert!(cache.get(&themed("icon13", 320)).is_some());
    }
}
