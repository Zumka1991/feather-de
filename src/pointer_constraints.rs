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

pub struct ActiveConstraint {
    pub surface: WlSurface,
    origin: Point<f64, Logical>,
    pub locked: bool,
    hint: Option<Point<f64, Logical>>,
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
            let live = with_pointer_constraint(&active.surface, &pointer, |c| {
                c.is_some_and(|c| c.is_active() && c.region().is_none_or(|r| r.rects.len() <= 128))
            });
            if !focused || !live {
                let active = self.active_constraint.take().unwrap();
                with_pointer_constraint(&active.surface, &pointer, |c| {
                    if let Some(c) = c {
                        if c.is_active() {
                            c.deactivate();
                        }
                    }
                });
                if focused {
                    if let Some(hint) = active.hint {
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
        let Some((under, origin)) =
            self.surface_under_pointer(self.pointer_location.x, self.pointer_location.y)
        else {
            return;
        };
        if under != surface {
            return;
        }
        let local = self.pointer_location - origin;
        let locked = with_pointer_constraint(&surface, &pointer, |constraint| {
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
            Some(matches!(&*c, PointerConstraint::Locked(_)))
        });
        if let Some(locked) = locked {
            self.active_constraint = Some(ActiveConstraint {
                surface,
                origin,
                locked,
                hint: None,
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
        pointer.frame(self);
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
        if let Some(c) = self
            .active_constraint
            .as_mut()
            .filter(|c| c.locked && c.surface == *surface)
        {
            // Hints are for unlocking. Warping while locked feeds game recentering back as motion.
            c.hint = Some(location);
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
}
