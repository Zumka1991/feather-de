//! A small logical output layout. Coordinates are relative to the primary display;
//! rendering stays per-output (there is no desktop-sized CPU framebuffer).
use crate::system_settings::Mode;
use smithay::{output::Output, reexports::wayland_server::backend::GlobalId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub primary: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}
impl Rect {
    pub fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x as f64
            && y >= self.y as f64
            && x < (self.x + self.w) as f64
            && y < (self.y + self.h) as f64
    }
    pub fn intersection(self, b: Self) -> i64 {
        let w = (self.x + self.w).min(b.x + b.w) - self.x.max(b.x);
        let h = (self.y + self.h).min(b.y + b.h) - self.y.max(b.y);
        w.max(0) as i64 * h.max(0) as i64
    }
    pub fn clamp(self, x: f64, y: f64) -> (f64, f64) {
        (
            x.clamp(self.x as f64, (self.x + self.w - 1) as f64),
            y.clamp(self.y as f64, (self.y + self.h - 1) as f64),
        )
    }
}
#[derive(Clone, Debug)]
pub struct Info {
    pub name: String,
    pub rect: Rect,
    pub mode: Mode,
    pub primary: bool,
}
pub struct Monitor {
    pub info: Info,
    pub output: Output,
    pub global: GlobalId,
}

pub fn project(rects: &[Rect], p: (f64, f64)) -> (f64, f64) {
    if rects.iter().any(|r| r.contains(p.0, p.1)) {
        return p;
    }
    rects
        .iter()
        .map(|r| r.clamp(p.0, p.1))
        .min_by(|a, b| {
            let dist = |q: (f64, f64)| (q.0 - p.0).powi(2) + (q.1 - p.1).powi(2);
            dist(*a).total_cmp(&dist(*b))
        })
        .unwrap_or(p)
}
pub fn owner(rects: &[Rect], window: Rect) -> usize {
    rects
        .iter()
        .enumerate()
        .max_by_key(|(i, r)| (r.intersection(window), std::cmp::Reverse(*i)))
        .map_or(0, |(i, _)| i)
}
/// Translate the layout so the primary screen's origin is zero. Returns the translation.
pub fn normalize(infos: &mut [Info]) -> (i32, i32) {
    let p = infos.iter().position(|m| m.primary).unwrap_or(0);
    let Some(m) = infos.get(p) else { return (0, 0) };
    let (dx, dy) = (-m.rect.x, -m.rect.y);
    for (i, m) in infos.iter_mut().enumerate() {
        m.primary = i == p;
        m.rect.x += dx;
        m.rect.y += dy;
    }
    (dx, dy)
}

/// Maximum bipartite matching; prefer unused CRTCs before disturbing an active one.
pub fn assign_crtcs(candidates: &[Vec<usize>], count: usize) -> Vec<Option<usize>> {
    fn visit(i: usize, c: &[Vec<usize>], owners: &mut [Option<usize>], seen: &mut [bool]) -> bool {
        for &r in &c[i] {
            if r < owners.len() && !seen[r] && owners[r].is_none() {
                seen[r] = true;
                owners[r] = Some(i);
                return true;
            }
        }
        for &r in &c[i] {
            if r >= owners.len() || seen[r] {
                continue;
            }
            seen[r] = true;
            if owners[r].is_some_and(|other| visit(other, c, owners, seen)) {
                owners[r] = Some(i);
                return true;
            }
        }
        false
    }
    let mut owners = vec![None; count];
    for i in 0..candidates.len() {
        visit(i, candidates, &mut owners, &mut vec![false; count]);
    }
    let mut out = vec![None; candidates.len()];
    for (r, i) in owners.into_iter().enumerate() {
        if let Some(i) = i {
            out[i] = Some(r);
        }
    }
    out
}

impl crate::FeatherCompositor {
    pub fn monitor_rects(&self) -> Vec<Rect> {
        self.monitors.iter().map(|m| m.info.rect).collect()
    }
    pub fn monitor_at(&self, x: f64, y: f64) -> Rect {
        self.monitors
            .iter()
            .find(|m| m.info.rect.contains(x, y))
            .or_else(|| self.monitors.iter().find(|m| m.info.primary))
            .map(|m| m.info.rect)
            .unwrap_or(Rect {
                x: 0,
                y: 0,
                w: self.shell.screen_w,
                h: self.shell.screen_h,
            })
    }
    pub fn window_monitor(&self, idx: usize) -> Rect {
        let w = &self.windows[idx];
        let rects = self.monitor_rects();
        rects
            .get(owner(
                &rects,
                Rect {
                    x: w.x,
                    y: w.y,
                    w: w.width,
                    h: w.total_height(),
                },
            ))
            .copied()
            .unwrap_or_else(|| self.monitor_at(0.0, 0.0))
    }
    pub fn primary_monitor(&self) -> Option<&Monitor> {
        self.monitors.iter().find(|m| m.info.primary)
    }
    /// Reconcile Wayland globals after a DRM rescan (also used by the nested fixture).
    pub fn sync_monitors(&mut self, specs: Vec<(String, Mode, (i32, i32))>) {
        let old: Vec<Info> = self.monitors.iter().map(|m| m.info.clone()).collect();
        let mut stale = std::mem::take(&mut self.monitors);
        for (name, mode, mm) in specs.into_iter().take(8) {
            let existing = stale
                .iter()
                .position(|m| m.info.name == name)
                .map(|i| stale.remove(i));
            let mut monitor = if let Some(m) = existing {
                m
            } else {
                let output = Output::new(
                    name.clone(),
                    smithay::output::PhysicalProperties {
                        size: mm.into(),
                        subpixel: smithay::output::Subpixel::Unknown,
                        make: "Feather DE".into(),
                        model: name.clone(),
                    },
                );
                let global = output.create_global::<Self>(&self.display_handle);
                let x = self
                    .monitors
                    .iter()
                    .map(|m| m.info.rect.x + m.info.rect.w)
                    .max()
                    .unwrap_or(0);
                let p = self.monitor_preferences.iter().find(|p| p.name == name);
                Monitor {
                    info: Info {
                        name: name.clone(),
                        rect: Rect {
                            x: p.map_or(x, |p| p.x),
                            y: p.map_or(0, |p| p.y),
                            w: 1,
                            h: 1,
                        },
                        mode,
                        primary: p.is_some_and(|p| p.primary),
                    },
                    output,
                    global,
                }
            };
            monitor.info.mode = mode;
            monitor.info.rect.w = (mode.width as f64 / self.scale_factor).round().max(1.0) as i32;
            monitor.info.rect.h = (mode.height as f64 / self.scale_factor).round().max(1.0) as i32;
            self.monitors.push(monitor);
        }
        for m in stale {
            for w in &self.windows {
                if let Some(s) = w.wl_surface() {
                    use smithay::wayland::compositor::{
                        with_surface_tree_downward, TraversalAction,
                    };
                    with_surface_tree_downward(
                        s,
                        (),
                        |_, _, _| TraversalAction::DoChildren(()),
                        |s, _, _| m.output.leave(s),
                        |_, _, _| true,
                    );
                    for (p, _) in smithay::desktop::PopupManager::popups_for_surface(s) {
                        m.output.leave(p.wl_surface());
                    }
                }
            }
            self.display_handle.disable_global::<Self>(m.global.clone());
            self.display_handle.remove_global::<Self>(m.global);
        }
        if !self.monitor_layout_loaded {
            self.monitor_layout_loaded = true;
            if let Some(name) = self
                .monitor_preferences
                .iter()
                .find(|p| p.primary && self.monitors.iter().any(|m| m.info.name == p.name))
                .map(|p| p.name.clone())
            {
                for m in &mut self.monitors {
                    m.info.primary = m.info.name == name;
                }
            }
        }
        if !self.monitors.iter().any(|m| m.info.primary) {
            if let Some(m) = self.monitors.first_mut() {
                m.info.primary = true;
            }
        }
        self.finish_monitor_change(&old);
    }
    pub fn finish_monitor_change(&mut self, old: &[Info]) {
        let mut infos: Vec<Info> = self.monitors.iter().map(|m| m.info.clone()).collect();
        let (dx, dy) = normalize(&mut infos);
        let primary = infos.iter().position(|m| m.primary).unwrap_or(0);
        let mut placed = Vec::new();
        if let Some(m) = infos.get(primary) {
            placed.push(m.rect);
        }
        for (i, m) in infos.iter_mut().enumerate() {
            if i == primary {
                continue;
            }
            if placed.iter().any(|r| m.rect.intersection(*r) > 0) {
                m.rect.x = placed.iter().map(|r| r.x + r.w).max().unwrap_or(0);
                m.rect.y = 0;
            }
            placed.push(m.rect);
        }
        for (m, info) in self.monitors.iter_mut().zip(infos) {
            m.info = info;
            let mode = smithay::output::Mode {
                size: (m.info.mode.width, m.info.mode.height).into(),
                refresh: m.info.mode.refresh,
            };
            m.output.change_current_state(
                Some(mode),
                Some(smithay::utils::Transform::Normal),
                Some(smithay::output::Scale::Fractional(self.scale_factor)),
                Some((m.info.rect.x, m.info.rect.y).into()),
            );
            m.output.set_preferred(mode);
        }
        if let Some(m) = self.primary_monitor() {
            let (output, r, mode) = (m.output.clone(), m.info.rect, m.info.mode);
            self.output = output;
            self.phys_w = mode.width;
            self.phys_h = mode.height;
            self.refresh_mhz = mode.refresh;
            if self.shell.screen_w != r.w || self.shell.screen_h != r.h {
                self.shell.reallocate_buffers(r.w, r.h);
            }
        }
        let rects = self.monitor_rects();
        let old_rects: Vec<_> = old.iter().map(|m| m.rect).collect();
        for win in &mut self.windows {
            let previous = old.get(owner(
                &old_rects,
                Rect {
                    x: win.x,
                    y: win.y,
                    w: win.width,
                    h: win.total_height(),
                },
            ));
            if let Some(before) = previous {
                if let Some(after) = self.monitors.iter().find(|m| m.info.name == before.name) {
                    let (tx, ty) = (
                        after.info.rect.x - before.rect.x,
                        after.info.rect.y - before.rect.y,
                    );
                    win.x += tx;
                    win.y += ty;
                    if let Some((x, y, _, _)) = &mut win.saved_rect {
                        *x += tx;
                        *y += ty;
                    }
                }
            }
            let r = Rect {
                x: win.x,
                y: win.y,
                w: win.width,
                h: win.total_height(),
            };
            if !rects.iter().any(|s| s.intersection(r) > 0) {
                if let Some(primary) = self.monitors.iter().find(|m| m.info.primary) {
                    win.x = primary.info.rect.x + 24;
                    win.y = primary.info.rect.y + crate::theme::TOP_BAR_HEIGHT + 24;
                }
            }
        }
        self.pointer_location.x += dx as f64;
        self.pointer_location.y += dy as f64;
        let p = project(&rects, (self.pointer_location.x, self.pointer_location.y));
        self.pointer_location = p.into();
        // A topology change cancels grabs and a pending mode confirmation tied to old indices.
        self.cancel_scene_input();
        self.icon_drag = None;
        self.screenshot_select = None;
        self.mission = None;
        self.shell.mission = None;
        self.shell.close_menus();
        self.pending_display = None;
        self.system_settings.confirming = false;
        self.system_settings.queued_mode = None;
        self.animations
            .set_enabled(self.animations.enabled(), &self.windows, &self.shell);
        if let (Some(name), crate::backend::Backend::Drm(d)) = (
            self.monitors
                .iter()
                .find(|m| m.info.primary)
                .map(|m| m.info.name.clone()),
            &mut self.backend,
        ) {
            d.sync_primary(&name);
        }
        self.lock
            .extra_surfaces
            .retain(|(n, _)| self.monitors.iter().any(|m| m.info.name == *n));
        self.refit_monitor_windows();
        self.refresh_output_membership();
        self.refresh_display_choices();
        self.lock_resized();
        self.lock.display_protected = false;
        self.lock.protected_frame = false;
        if let crate::backend::Backend::Winit(b) = &self.backend {
            self.damage_tracker = smithay::backend::renderer::damage::OutputDamageTracker::new(
                b.window_size(),
                1.0,
                smithay::utils::Transform::Flipped180,
            );
        }
        self.pointer_focus_dirty = true;
        self.refresh_focus();
        self.mark_settings_dirty();
        self.needs_redraw = true;
    }
    pub fn refit_monitor_windows(&mut self) {
        let rects = self.monitor_rects();
        for w in &mut self.windows {
            if let Some((x, y, width, height)) = &mut w.saved_rect {
                if !rects.iter().any(|r| {
                    r.intersection(Rect {
                        x: *x,
                        y: *y,
                        w: *width,
                        h: *height,
                    }) > 0
                }) {
                    *x = 24;
                    *y = crate::theme::TOP_BAR_HEIGHT + 24;
                }
            }
            let Some(r) = rects
                .get(owner(
                    &rects,
                    Rect {
                        x: w.x,
                        y: w.y,
                        w: w.width,
                        h: w.total_height(),
                    },
                ))
                .copied()
            else {
                continue;
            };
            if w.fullscreen {
                w.x = r.x;
                w.y = r.y;
                w.resize_client(r.w, r.h);
            } else if w.maximized {
                let (x, y, ww, hh) = if r.x == 0 && r.y == 0 {
                    crate::theme::maximized_area(r.w, r.h)
                } else {
                    (0, 0, r.w, r.h)
                };
                w.x = r.x + x;
                w.y = r.y + y;
                w.resize_client(ww, hh - w.titlebar_h());
            } else {
                // Keep the titlebar reachable after a smaller mode or a disconnected screen.
                w.x = w.x.clamp(r.x - w.width + 80, r.x + r.w - 80);
                w.y = w.y.clamp(
                    r.y + if r.x == 0 && r.y == 0 {
                        crate::theme::TOP_BAR_HEIGHT
                    } else {
                        0
                    },
                    (r.y + r.h - 40).max(r.y + crate::theme::TOP_BAR_HEIGHT),
                );
            }
        }
    }
    pub fn refresh_output_membership(&self) {
        use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};
        for w in &self.windows {
            let r = Rect {
                x: w.x,
                y: w.y,
                w: w.width,
                h: w.total_height(),
            };
            if let Some(s) = w.wl_surface() {
                for m in &self.monitors {
                    let inside = w.is_visible() && m.info.rect.intersection(r) > 0;
                    with_surface_tree_downward(
                        s,
                        (),
                        |_, _, _| TraversalAction::DoChildren(()),
                        |s, _, _| {
                            if inside {
                                m.output.enter(s)
                            } else {
                                m.output.leave(s)
                            }
                        },
                        |_, _, _| true,
                    );
                    for (p, _) in smithay::desktop::PopupManager::popups_for_surface(s) {
                        if inside {
                            m.output.enter(p.wl_surface())
                        } else {
                            m.output.leave(p.wl_surface())
                        }
                    }
                }
            }
        }
    }
    pub fn select_primary_monitor(&mut self, name: &str) {
        if self.pending_display.is_some() || !self.monitors.iter().any(|m| m.info.name == name) {
            return;
        }
        let old: Vec<Info> = self.monitors.iter().map(|m| m.info.clone()).collect();
        for m in &mut self.monitors {
            m.info.primary = m.info.name == name;
        }
        self.finish_monitor_change(&old);
        self.remember_monitor_layout();
    }
    pub fn position_monitor(&mut self, name: &str, side: u8) {
        if self.pending_display.is_some() {
            return;
        }
        let old: Vec<Info> = self.monitors.iter().map(|m| m.info.clone()).collect();
        let Some(primary) = self.primary_monitor().map(|m| m.info.rect) else {
            return;
        };
        if let Some(m) = self
            .monitors
            .iter_mut()
            .find(|m| m.info.name == name && !m.info.primary)
        {
            (m.info.rect.x, m.info.rect.y) = match side {
                0 => (primary.x - m.info.rect.w, 0),
                1 => (primary.x + primary.w, 0),
                2 => (0, primary.y - m.info.rect.h),
                _ => (0, primary.y + primary.h),
            };
        }
        // Do not accept overlapping outputs: a point must identify one display.
        let rects = self.monitor_rects();
        if rects
            .iter()
            .enumerate()
            .any(|(i, r)| rects.iter().skip(i + 1).any(|b| r.intersection(*b) > 0))
        {
            for (m, info) in self.monitors.iter_mut().zip(&old) {
                m.info = info.clone();
            }
            self.system_settings.error = Some("Это место занято другим дисплеем".into());
            return;
        }
        self.finish_monitor_change(&old);
        self.remember_monitor_layout();
    }
    fn remember_monitor_layout(&mut self) {
        for m in &self.monitors {
            let p = Placement {
                name: m.info.name.clone(),
                x: m.info.rect.x,
                y: m.info.rect.y,
                primary: m.info.primary,
            };
            if let Some(old) = self
                .monitor_preferences
                .iter_mut()
                .find(|x| x.name == p.name)
            {
                *old = p
            } else {
                self.monitor_preferences.push(p);
            }
        }
        // A previously unplugged primary must not take the role away on reconnection.
        let primary = self.primary_monitor().map(|m| m.info.name.clone());
        for p in &mut self.monitor_preferences {
            p.primary = Some(&p.name) == primary.as_ref();
        }
        self.monitor_preferences.truncate(16);
        self.save_settings();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crtcs_are_unique_and_flexible_connectors_do_not_starve_restricted_ones() {
        assert_eq!(
            assign_crtcs(&[vec![0, 1], vec![0]], 2),
            vec![Some(1), Some(0)]
        );
        assert_eq!(
            assign_crtcs(&[vec![1, 0], vec![0]], 2),
            vec![Some(1), Some(0)]
        );
        assert_eq!(
            assign_crtcs(&[vec![0], vec![0], vec![]], 1),
            vec![Some(0), None, None]
        );
    }
    #[test]
    fn pointer_projects_to_real_outputs_instead_of_desktop_gaps() {
        let rs = [
            Rect {
                x: -1280,
                y: 200,
                w: 1280,
                h: 720,
            },
            Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
        ];
        assert_eq!(project(&rs, (-20.0, 100.0)), (0.0, 100.0));
        assert_eq!(project(&rs, (-900.0, 400.0)), (-900.0, 400.0));
        assert_eq!(project(&rs, (-2000.0, 1000.0)), (-1280.0, 919.0));
        assert_eq!(
            owner(
                &rs,
                Rect {
                    x: -100,
                    y: 300,
                    w: 600,
                    h: 500
                }
            ),
            1
        );
        assert_eq!(
            owner(
                &rs,
                Rect {
                    x: -600,
                    y: 300,
                    w: 700,
                    h: 500
                }
            ),
            0
        );
    }
    #[test]
    fn primary_change_preserves_relative_positions_and_negative_coordinates() {
        let mode = Mode {
            width: 1920,
            height: 1080,
            refresh: 60000,
        };
        let mut ms = vec![
            Info {
                name: "A".into(),
                rect: Rect {
                    x: 0,
                    y: 0,
                    w: 1920,
                    h: 1080,
                },
                mode,
                primary: false,
            },
            Info {
                name: "B".into(),
                rect: Rect {
                    x: 1920,
                    y: -200,
                    w: 1280,
                    h: 720,
                },
                mode,
                primary: true,
            },
        ];
        assert_eq!(normalize(&mut ms), (-1920, 200));
        assert_eq!(ms[1].rect.x, 0);
        assert_eq!(ms[0].rect.x, -1920);
        assert_eq!(ms[0].rect.y, 200);
        assert_eq!(normalize(&mut ms), (0, 0));
    }
}

/// End-to-end scenarios operate only inside the owned nested compositor.
pub fn install_checks(
    state: &mut crate::FeatherCompositor,
) -> Result<(), Box<dyn std::error::Error>> {
    let Ok(mode) = std::env::var("FEATHER_DEBUG_MONITORS") else {
        return Ok(());
    };
    if state.backend.is_drm() || !state.debug_input_only {
        return Err("monitor check requires isolated nested input".into());
    }
    fn at(
        s: &crate::FeatherCompositor,
        ms: u64,
        mut f: impl FnMut(&mut crate::FeatherCompositor) + 'static,
    ) -> Result<(), Box<dyn std::error::Error>> {
        s.loop_handle.insert_source(
            smithay::reexports::calloop::timer::Timer::from_duration(
                std::time::Duration::from_millis(ms),
            ),
            move |_, _, s| {
                f(s);
                smithay::reexports::calloop::timer::TimeoutAction::Drop
            },
        )?;
        Ok(())
    }
    fn add(s: &mut crate::FeatherCompositor) {
        let mode = s.monitors.first().unwrap().info.mode;
        s.sync_monitors(vec![
            ("Feather-Display-1".into(), mode, (600, 340)),
            ("Virtual-2".into(), mode, (520, 290)),
        ]);
    }
    fn preview(s: &mut crate::FeatherCompositor, name: Option<&str>, file: &str) {
        s.preview_monitor = name.map(str::to_owned);
        if let crate::backend::Backend::Winit(b) = &s.backend {
            s.damage_tracker = smithay::backend::renderer::damage::OutputDamageTracker::new(
                b.window_size(),
                1.0,
                smithay::utils::Transform::Flipped180,
            );
        }
        if let Some(dir) = std::env::var_os("FEATHER_DEBUG_MONITOR_DIR") {
            s.screenshot_request = Some(std::path::PathBuf::from(dir).join(file));
        }
        s.needs_redraw = true;
    }
    fn terminal(s: &crate::FeatherCompositor) -> usize {
        s.windows
            .iter()
            .position(|w| w.app_id == "feather-terminal" && w.mapped)
            .expect("owned terminal mapped")
    }
    fn maximize(s: &mut crate::FeatherCompositor, on: bool) {
        use smithay::wayland::shell::xdg::XdgShellHandler;
        let t = s.windows[terminal(s)].toplevel().unwrap().clone();
        if on {
            s.maximize_request(t)
        } else {
            s.unmaximize_request(t)
        }
    }
    fn fullscreen(s: &mut crate::FeatherCompositor, on: bool) {
        use smithay::wayland::shell::xdg::XdgShellHandler;
        let t = s.windows[terminal(s)].toplevel().unwrap().clone();
        if on {
            s.fullscreen_request(t, None)
        } else {
            s.unfullscreen_request(t)
        }
    }
    at(state, 400, add)?;
    if mode == "restore" {
        at(state, 1200, |s| {
            assert_eq!(s.primary_monitor().unwrap().info.name, "Virtual-2");
            assert!(s
                .monitors
                .iter()
                .any(|m| !m.info.primary && m.info.rect.x > 0));
            eprintln!("[feather-monitor-check] restored: ok");
        })?;
        return Ok(());
    }
    if mode == "external" {
        at(state, 1800, |s| {
            let r = s
                .monitors
                .iter()
                .find(|m| m.info.name == "Virtual-2")
                .unwrap()
                .info
                .rect;
            s.debug_pointer_motion((r.x + 100) as f64, (r.y + 100) as f64);
        })?;
        at(state, 2100, |s| {
            s.debug_pointer_button(0x110, true);
            s.debug_pointer_button(0x110, false);
        })?;
        at(state, 2600, |s| {
            assert!(s.lock.active);
            assert_eq!(s.lock.extra_surfaces.len(), 2);
            preview(s, Some("Virtual-2"), "external-secondary.png");
            eprintln!("[feather-monitor-check] external surfaces: ok");
        })?;
        at(state, 4800, |s| {
            assert!(
                s.lock.active && !s.lock.external(),
                "owner death stays locked"
            );
            preview(s, Some("Virtual-2"), "external-fallback.png");
            eprintln!("[feather-monitor-check] complete: ok");
        })?;
        return Ok(());
    }
    at(state, 1200, |s| {
        let i = terminal(s);
        let (x, y) = (s.windows[i].x + 120, s.windows[i].y + 80);
        let r = s
            .monitors
            .iter()
            .find(|m| m.info.name == "Virtual-2")
            .unwrap()
            .info
            .rect;
        s.debug_pointer_motion(x as f64, y as f64);
        // Feather Terminal requests CSD: move through the compositor's Super+drag,
        // rather than assuming it has a server-side titlebar.
        s.debug_keyboard_key(125, true);
        s.debug_pointer_button(0x110, true);
        assert!(
            s.active_drag.is_some(),
            "Super+drag starts on the owned client"
        );
        s.debug_pointer_motion((r.x + 180) as f64, (r.y + 100) as f64);
        s.debug_pointer_button(0x110, false);
        s.debug_keyboard_key(125, false);
        assert_eq!(s.window_monitor(terminal(s)), r);
        maximize(s, true);
    })?;
    at(state, 2400, |s| {
        let i = terminal(s);
        let r = s.window_monitor(i);
        let w = &s.windows[i];
        assert!(w.maximized);
        assert_eq!((w.x, w.y, w.width, w.total_height()), (r.x, r.y, r.w, r.h));
        fullscreen(s, true);
        eprintln!("[feather-monitor-check] drag and maximize: ok");
    })?;
    at(state, 3200, |s| {
        let i = terminal(s);
        let r = s.window_monitor(i);
        let w = &s.windows[i];
        assert!(w.fullscreen);
        assert_eq!((w.x, w.y, w.width, w.height), (r.x, r.y, r.w, r.h));
        preview(s, Some("Virtual-2"), "secondary.png");
    })?;
    at(state, 3800, |s| {
        fullscreen(s, false);
        maximize(s, false);
    })?;
    at(state, 4300, |s| {
        s.apply_system_setting(crate::system_settings::Action::PositionMonitor(
            "Virtual-2".into(),
            0,
        ));
    })?;
    at(state, 4800, |s| {
        let i = terminal(s);
        let w = &s.windows[i];
        assert!(w.x < 0, "window follows monitor moved left");
        assert!(
            s.surface_under_pointer((w.x + 100) as f64, (w.y + w.titlebar_h() + 50) as f64)
                .is_some(),
            "negative global pointer reaches client"
        );
        s.apply_system_setting(crate::system_settings::Action::PrimaryMonitor(
            "Virtual-2".into(),
        ));
        eprintln!("[feather-monitor-check] negative coordinates and primary: ok");
    })?;
    at(state, 5600, |s| {
        assert_eq!(s.primary_monitor().unwrap().info.name, "Virtual-2");
        s.apply_system_setting(crate::system_settings::Action::Page(
            crate::system_settings::Page::Displays,
        ));
        let i = s
            .windows
            .iter()
            .position(|w| w.app_id == "builtin.settings")
            .unwrap();
        let rect = s.window_monitor(i);
        let name = s
            .monitors
            .iter()
            .find(|m| m.info.rect == rect)
            .unwrap()
            .info
            .name
            .clone();
        preview(s, Some(&name), "settings.png");
    })?;
    at(state, 6800, |s| {
        maximize(s, true);
        let m = s
            .monitors
            .iter()
            .find(|m| m.info.name != "Virtual-2")
            .unwrap()
            .info
            .clone();
        s.sync_monitors(vec![(m.name, m.mode, (600, 340))]);
        let i = terminal(s);
        let w = &s.windows[i];
        assert_eq!(s.monitors.len(), 1);
        assert!(
            s.window_monitor(i).intersection(Rect {
                x: w.x,
                y: w.y,
                w: w.width,
                h: w.total_height()
            }) > 0
        );
        eprintln!("[feather-monitor-check] disconnect recovers window: ok");
    })?;
    at(state, 7300, |s| {
        let i = terminal(s);
        assert!(s.windows[i].maximized);
        assert_eq!(s.windows[i].x, 0);
        assert_eq!(s.windows[i].width, s.shell.screen_w);
        maximize(s, false);
    })?;
    at(state, 7400, add)?;
    at(state, 8000, |s| {
        assert_eq!(s.monitors.len(), 2);
        s.lock_session();
    })?;
    at(state, 8700, |s| {
        preview(s, Some("Virtual-2"), "locked-secondary.png");
    })?;
    at(state, 9400, |s| {
        assert!(s.lock.active);
        assert!(s.surface_under_pointer(100.0, 100.0).is_none());
        let m = s
            .monitors
            .iter()
            .find(|m| !m.info.primary)
            .unwrap()
            .info
            .clone();
        s.sync_monitors(vec![(m.name, m.mode, (600, 340))]);
        assert!(s.lock.active);
    })?;
    at(state, 10000, add)?;
    at(state, 10800, |s| {
        assert!(s.lock.active);
        assert_eq!(s.monitors.len(), 2);
        let name = s
            .monitors
            .iter()
            .find(|m| !m.info.primary)
            .unwrap()
            .info
            .name
            .clone();
        preview(s, Some(&name), "locked-reconnected.png");
        eprintln!("[feather-monitor-check] complete: ok");
    })?;
    Ok(())
}
