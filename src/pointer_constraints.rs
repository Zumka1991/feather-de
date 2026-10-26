//! Game pointer capture. All coordinates here are compositor logical coordinates.
use crate::FeatherCompositor;
use smithay::{
    backend::renderer::utils::with_renderer_surface_state,
    input::pointer::{ButtonEvent, MotionEvent, PointerHandle, RelativeMotionEvent},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle, Size, SERIAL_COUNTER},
    wayland::compositor::{with_states, RegionAttributes, SurfaceAttributes},
    wayland::pointer_constraints::{
        with_pointer_constraint, PointerConstraint, PointerConstraintsHandler,
    },
};

#[derive(Default)]
struct X11WarpHint(std::sync::Mutex<Option<Point<f64, Logical>>>);

pub struct ActiveConstraint {
    pub surface: WlSurface,
    origin: Point<f64, Logical>,
    pub locked: bool,
    x11_warp_lock: bool,
    hint: Option<Point<f64, Logical>>,
}

fn clamp_to_size(p: Point<f64, Logical>, size: Size<i32, Logical>) -> Point<f64, Logical> {
    if size.w <= 0 || size.h <= 0 {
        return p;
    }
    let max_x = (size.w - 1).max(0) as f64;
    let max_y = (size.h - 1).max(0) as f64;
    Point::from((p.x.clamp(0.0, max_x), p.y.clamp(0.0, max_y)))
}

fn confine_segment(
    old: Point<f64, Logical>,
    requested: Point<f64, Logical>,
    size: Size<i32, Logical>,
    region: Option<&RegionAttributes>,
    input: Option<&RegionAttributes>,
) -> Point<f64, Logical> {
    let bounds = Rectangle::from_size(size);
    let contains = |p: Point<f64, Logical>| {
        bounds.contains(p.to_i32_floor())
            && region.is_none_or(|r| r.contains(p.to_i32_floor()))
            && input.is_none_or(|r| r.contains(p.to_i32_floor()))
    };
    if !contains(old) {
        return old;
    }
    let delta = requested - old;
    let at = |t: f64| Point::from((old.x + delta.x * t, old.y + delta.y * t));
    let epsilon = 0.0001 / delta.x.abs().max(delta.y.abs()).max(1.0);
    let mut end: f64 = 1.0;
    // Check every boundary crossing, including holes and disconnected regions.
    // Merely clamping the endpoint permits tunnelling across excluded rectangles.
    for rect in std::iter::once(&bounds).chain(
        region
            .into_iter()
            .chain(input)
            .flat_map(|r| r.rects.iter().map(|(_, r)| r)),
    ) {
        for (start, d, min, max) in [
            (old.x, delta.x, rect.loc.x, rect.loc.x + rect.size.w),
            (old.y, delta.y, rect.loc.y, rect.loc.y + rect.size.h),
        ] {
            if d == 0.0 {
                continue;
            }
            for boundary in [min, max] {
                let t = (boundary as f64 - start) / d;
                if (0.0..=end).contains(&t) && !contains(at((t + epsilon).min(1.0))) {
                    end = t;
                }
            }
        }
    }
    if end < 1.0 || !contains(requested) {
        at((end - epsilon).max(0.0))
    } else {
        requested
    }
}

impl FeatherCompositor {
    fn set_x11_warp_hint(surface: &WlSurface, hint: Option<Point<f64, Logical>>) {
        with_states(surface, |states| {
            *states
                .data_map
                .get_or_insert(X11WarpHint::default)
                .0
                .lock()
                .unwrap() = hint;
        });
    }

    fn get_x11_warp_hint(surface: &WlSurface) -> Option<Point<f64, Logical>> {
        with_states(surface, |states| {
            states
                .data_map
                .get::<X11WarpHint>()
                .and_then(|h| *h.0.lock().unwrap())
        })
    }

    fn constraint_surface_geometry(
        &self,
        surface: &WlSurface,
    ) -> Option<(Point<f64, Logical>, Size<i32, Logical>)> {
        let size = with_renderer_surface_state(surface, |s| s.surface_size()).flatten();
        if let Some((under, origin)) =
            self.surface_under_pointer(self.pointer_location.x, self.pointer_location.y)
        {
            if &under == surface {
                let size = size.or_else(|| {
                    self.windows
                        .iter()
                        .find(|w| w.is_visible() && w.wl_surface() == Some(surface))
                        .map(|w| Size::from((w.width, w.height)))
                })?;
                return Some((origin, size));
            }
        }
        if crate::xwayland::is_xwayland_surface(surface) {
            let win = self
                .windows
                .iter()
                .rev()
                .find(|w| w.is_visible() && w.wl_surface() == Some(surface))?;
            let origin = win.surface_origin().to_f64();
            let size = size.unwrap_or_else(|| Size::from((win.width, win.height)));
            return Some((origin, size));
        }
        None
    }

    fn constraint_allowed(&self, surface: &WlSurface) -> bool {
        !self.pointer_capture_suspended
            && !self.lock.active
            && !self.auth.is_open()
            && !self.mission_open()
            && !self.shell.spotlight_open
            && !self.shell.control_center_open
            && self.shell.open_menu.is_none()
            && self.shell.dock_menu.is_none()
            && !self.shell.center_open
            && self.shell.launchpad.is_none()
            && self.screenshot_select.is_none()
            && self.active_drag.is_none()
            && !self.shell.switcher.as_ref().is_some_and(|s| s.visible)
            && self.keyboard.current_focus().as_ref() == self.owner_of_surface(surface).0.as_ref()
            && self.keyboard.current_focus().is_some()
    }

    pub fn pointer_locked(&self) -> bool {
        self.active_constraint.as_ref().is_some_and(|c| c.locked)
    }

    /// Also runs after dispatch: clients may destroy a lock without moving the pointer.
    pub fn sync_pointer_constraint(&mut self) {
        let pointer = self.pointer.clone();
        if let Some(active) = self.active_constraint.as_ref() {
            let focused = pointer.current_focus().as_ref() == Some(&active.surface)
                && self.constraint_allowed(&active.surface);
            let protocol_live = with_pointer_constraint(&active.surface, &pointer, |c| {
                c.is_some_and(|c| c.is_active() && c.region().is_none_or(|r| r.rects.len() <= 128))
            });
            // In Wine/Proton (e.g. Lineage 2 RMB camera rotation), SetCursorPos + ClipCursor
            // creates a short-lived XWayland pointer_warp_emulator (LockedPointer) and then
            // grabs an InputOnly clip_window, which destroys LockedPointer without creating
            // ConfinedPointer. Keep the relative-only lock alive until the held button is released.
            let has_pending =
                with_pointer_constraint(&active.surface, &pointer, |c| c.is_some());
            if focused
                && !protocol_live
                && !has_pending
                && active.x11_warp_lock
                && !self.pressed_buttons.is_empty()
            {
                return;
            }
            let live = protocol_live && !(active.x11_warp_lock && self.pressed_buttons.is_empty());
            if !focused || !live {
                let active = self.active_constraint.take().unwrap();
                let is_x11 = crate::xwayland::is_xwayland_surface(&active.surface);
                with_pointer_constraint(&active.surface, &pointer, |c| {
                    if let Some(c) = c {
                        if c.is_active() {
                            c.deactivate();
                        }
                    }
                });
                if !focused || self.pressed_buttons.is_empty() {
                    Self::set_x11_warp_hint(&active.surface, None);
                }
                if focused {
                    if !protocol_live
                        && !has_pending
                        && is_x11
                        && active.locked
                        && active.hint.is_some()
                        && !self.pressed_buttons.is_empty()
                    {
                        self.active_constraint = Some(ActiveConstraint {
                            surface: active.surface,
                            origin: active.origin,
                            locked: true,
                            x11_warp_lock: true,
                            hint: active.hint,
                        });
                        return;
                    }
                    if let Some(hint) = active.hint {
                        if is_x11 {
                            // When a warp was part of a button-held camera drag (x11_warp_lock),
                            // pointer_location stayed at SavedCursor where the button was pressed.
                            // Only apply the hint to pointer_location for standalone warps.
                            if !active.x11_warp_lock {
                                let hint = self
                                    .constraint_surface_geometry(&active.surface)
                                    .map(|(_, size)| clamp_to_size(hint, size))
                                    .unwrap_or(hint);
                                self.pointer_location = active.origin + hint;
                            }
                        } else {
                            let target = active.origin + hint;
                            if self
                                .surface_under_pointer(target.x, target.y)
                                .as_ref()
                                .is_some_and(|(s, _)| *s == active.surface)
                            {
                                self.pointer_location = target;
                                pointer.motion(
                                    self,
                                    Some((active.surface, active.origin)),
                                    &MotionEvent {
                                        location: target,
                                        serial: SERIAL_COUNTER.next_serial(),
                                        time: self.start_time.elapsed().as_millis() as u32,
                                    },
                                );
                                pointer.frame(self);
                            }
                        }
                    }
                } else {
                    // A held game button must not keep the implicit grab after Alt+Tab/lock.
                    let time = self.start_time.elapsed().as_millis() as u32;
                    let held: Vec<_> = self
                        .pressed_buttons
                        .iter()
                        .copied()
                        .filter(|b| !self.consumed_buttons.contains(b))
                        .collect();
                    for button in held {
                        pointer.button(
                            self,
                            &ButtonEvent {
                                button,
                                state: smithay::backend::input::ButtonState::Released,
                                serial: SERIAL_COUNTER.next_serial(),
                                time,
                            },
                        );
                        self.consumed_buttons.push(button);
                    }
                    pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), time);
                    pointer.motion(
                        self,
                        None,
                        &MotionEvent {
                            location: self.pointer_location,
                            serial: SERIAL_COUNTER.next_serial(),
                            time,
                        },
                    );
                    pointer.frame(self);
                    self.pointer_focus_dirty = true;
                }
                self.apply_cursor(self.cursor.status.clone());
                self.needs_redraw = true;
            }
        }
        if self.active_constraint.is_some() {
            return;
        }
        let Some(surface) = pointer.current_focus() else {
            return;
        };
        if !self.constraint_allowed(&surface) {
            return;
        }
        let Some((origin, size)) = self.constraint_surface_geometry(&surface) else {
            return;
        };
        let is_x11 = crate::xwayland::is_xwayland_surface(&surface);
        let local = if is_x11 {
            clamp_to_size(self.pointer_location - origin, size)
        } else {
            self.pointer_location - origin
        };
        let x11_warp_hint = if is_x11 && !self.pressed_buttons.is_empty() {
            Self::get_x11_warp_hint(&surface)
        } else {
            None
        };
        let activated = with_pointer_constraint(&surface, &pointer, |constraint| {
            let c = constraint?;
            if c.region().is_some_and(|r| r.rects.len() > 128) {
                return None;
            }
            if c.region()
                .is_some_and(|r| !r.contains(local.to_i32_floor()))
            {
                return None;
            }
            c.activate();
            Some(match &*c {
                PointerConstraint::Locked(l) => {
                    let h = l.cursor_position_hint();
                    (
                        true,
                        is_x11 && h.is_some() && !self.pressed_buttons.is_empty(),
                        h,
                    )
                }
                PointerConstraint::Confined(_) => (
                    x11_warp_hint.is_some(),
                    x11_warp_hint.is_some(),
                    x11_warp_hint,
                ),
            })
        });
        if let Some((locked, x11_warp_lock, mut hint)) = activated {
            if is_x11 {
                if let Some(h) = hint {
                    let clamped = clamp_to_size(h, size);
                    hint = Some(clamped);
                    if !self.pressed_buttons.is_empty() {
                        Self::set_x11_warp_hint(&surface, Some(clamped));
                    } else {
                        self.pointer_location = origin + clamped;
                    }
                } else {
                    self.pointer_location = origin + local;
                }
            }
            self.active_constraint = Some(ActiveConstraint {
                surface,
                origin,
                locked,
                x11_warp_lock,
                hint,
            });
            self.apply_cursor(self.cursor.status.clone());
            self.needs_redraw = true;
        }
    }

    pub fn game_relative_motion(
        &mut self,
        delta: Point<f64, Logical>,
        unaccel: Point<f64, Logical>,
        utime: u64,
    ) {
        self.sync_pointer_constraint();
        let pointer = self.pointer.clone();
        let focus = self
            .active_constraint
            .as_ref()
            .map(|c| (c.surface.clone(), c.origin))
            .or_else(|| {
                self.surface_under_pointer(self.pointer_location.x, self.pointer_location.y)
            });
        pointer.relative_motion(
            self,
            focus,
            &RelativeMotionEvent {
                delta,
                delta_unaccel: unaccel,
                utime,
            },
        );
        // When unlocked, on_pointer_motion flushes both relative_motion and motion in a single
        // wl_pointer.frame so XWayland sees has_relative == true and sets POINTER_NORAW.
        if self.pointer_locked() {
            pointer.frame(self);
        }
    }

    pub fn constrained_position(&mut self, requested: Point<f64, Logical>) -> Point<f64, Logical> {
        self.sync_pointer_constraint();
        let Some(active) = self.active_constraint.as_ref() else {
            return requested;
        };
        if active.locked {
            return self.pointer_location;
        }
        let origin = active.origin;
        let size = with_renderer_surface_state(&active.surface, |s| s.surface_size()).flatten();
        let Some(size) = size else {
            return self.pointer_location;
        };
        // with_pointer_constraint also locks SurfaceData: never nest it in with_states.
        // Clip each region in turn; the earliest boundary of their intersection wins.
        let clipped = with_states(&active.surface, |states| {
            let mut attrs = states.cached_state.get::<SurfaceAttributes>();
            let input = attrs.current().input_region.as_ref();
            if input.is_some_and(|r| r.rects.len() > 128) {
                return self.pointer_location - origin;
            }
            confine_segment(
                self.pointer_location - origin,
                requested - origin,
                size,
                None,
                input,
            )
        });
        with_pointer_constraint(&active.surface, &self.pointer, |c| {
            confine_segment(
                self.pointer_location - origin,
                clipped,
                size,
                c.as_ref().and_then(|c| c.region()),
                None,
            ) + origin
        })
    }

    pub(crate) fn debug_x11_rmb_warp_check(
        &mut self,
        surface: &WlSurface,
        origin: Point<f64, Logical>,
    ) {
        let pointer = self.pointer.clone();
        let saved = self.pointer_location;
        self.active_constraint = Some(ActiveConstraint {
            surface: surface.clone(),
            origin,
            locked: true,
            x11_warp_lock: false,
            hint: None,
        });
        PointerConstraintsHandler::cursor_position_hint(
            self,
            surface,
            &pointer,
            (380.0, 220.0).into(),
        );
        assert_eq!(self.pointer_location, saved);
        assert_eq!(
            Self::get_x11_warp_hint(surface),
            Some(Point::from((380.0, 220.0)))
        );
        // Simulate XWayland destroying its 5 ms pointer_warp_emulator while RMB is still held
        // (and Wine's InputOnly clip_window prevents ConfinedPointer): lock must stay active.
        self.sync_pointer_constraint();
        assert!(self.pointer_locked());
        assert_eq!(
            self.constrained_position(origin + Point::from((700.0, 400.0))),
            saved
        );
        // Releasing RMB must immediately end x11_warp_lock and leave pointer at SavedCursor.
        self.pressed_buttons.clear();
        self.sync_pointer_constraint();
        assert!(!self.pointer_locked());
        assert_eq!(self.pointer_location, saved);
        assert_eq!(Self::get_x11_warp_hint(surface), None);
        // A standalone XWayland warp without held buttons updates pointer_location to the clamped hint.
        self.active_constraint = Some(ActiveConstraint {
            surface: surface.clone(),
            origin,
            locked: true,
            x11_warp_lock: false,
            hint: None,
        });
        PointerConstraintsHandler::cursor_position_hint(
            self,
            surface,
            &pointer,
            (9999.0, -50.0).into(),
        );
        self.sync_pointer_constraint();
        assert!(!self.pointer_locked());
        assert_eq!(self.pointer_location, origin + Point::from((759.0, 0.0)));
    }
}

impl PointerConstraintsHandler for FeatherCompositor {
    fn new_constraint(&mut self, surface: &WlSurface, _pointer: &PointerHandle<Self>) {
        tracing::debug!(
            allowed = self.constraint_allowed(surface),
            "pointer constraint requested"
        );
        self.sync_pointer_constraint();
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        if self.active_constraint.is_none() {
            self.sync_pointer_constraint();
        }
        let is_x11 = crate::xwayland::is_xwayland_surface(surface);
        let clamped = if is_x11 {
            self.constraint_surface_geometry(surface)
                .map(|(_, size)| clamp_to_size(location, size))
                .unwrap_or(location)
        } else {
            location
        };
        let buttons_held = !self.pressed_buttons.is_empty();
        if let Some(c) = self
            .active_constraint
            .as_mut()
            .filter(|c| c.locked && c.surface == *surface)
        {
            // Hints are for unlocking. Warping while locked feeds game recentering back as motion.
            c.hint = Some(clamped);
            if is_x11 {
                if buttons_held {
                    c.x11_warp_lock = true;
                    Self::set_x11_warp_hint(surface, Some(clamped));
                } else {
                    let origin = c.origin;
                    self.pointer_location = origin + clamped;
                }
            }
        }
    }
}

smithay::delegate_relative_pointer!(FeatherCompositor);
smithay::delegate_pointer_constraints!(FeatherCompositor);

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::wayland::compositor::RectangleKind;
    #[test]
    fn confinement_stops_at_edges_and_does_not_cross_holes_or_disconnected_regions() {
        let size = (640, 360).into();
        let p = confine_segment(
            (150.0, 100.0).into(),
            (900.0, 500.0).into(),
            size,
            None,
            None,
        );
        assert!(p.x >= 0.0 && p.x < 640.0 && p.y >= 0.0 && p.y < 360.0);
        let region = RegionAttributes {
            rects: vec![
                (RectangleKind::Add, Rectangle::from_size(size)),
                (
                    RectangleKind::Subtract,
                    Rectangle::new((200, 0).into(), (20, 360).into()),
                ),
            ],
        };
        let p = confine_segment(
            (150.0, 100.0).into(),
            (400.0, 100.0).into(),
            size,
            Some(&region),
            None,
        );
        assert!(p.x < 200.0 && p.x > 199.9);
        assert_eq!(p.y, 100.0);
        let region = RegionAttributes {
            rects: vec![
                (
                    RectangleKind::Add,
                    Rectangle::new((10, 10).into(), (50, 50).into()),
                ),
                (
                    RectangleKind::Add,
                    Rectangle::new((100, 10).into(), (50, 50).into()),
                ),
            ],
        };
        let p = confine_segment(
            (30.0, 30.0).into(),
            (120.0, 30.0).into(),
            size,
            Some(&region),
            None,
        );
        assert!(p.x < 60.0 && p.x > 59.9);
    }

    #[test]
    fn clamp_to_size_keeps_warped_points_inside_surface_bounds() {
        let size: Size<i32, Logical> = (640, 360).into();
        let clamped = clamp_to_size((-15.0, 720.0).into(), size);
        assert_eq!(clamped, Point::from((0.0, 359.0)));
        assert!(Rectangle::from_size(size).contains(clamped.to_i32_floor()));
        let inside = clamp_to_size((320.5, 180.25).into(), size);
        assert_eq!(inside, Point::from((320.5, 180.25)));
    }
}
