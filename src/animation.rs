//! Frame-driven spring motion. No animation timers and no rasterized intermediate frames.
use std::{collections::HashMap, time::Instant};

use crate::{
    shell::{MacShell, OverlayKind, R},
    window::ManagedWindow,
};

/// Analytic critically damped spring (frequency is physical, not a transition duration).
/// The exact solution is stable at any refresh rate and preserves velocity on reversal.
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    pub value: f64,
    velocity: f64,
    target: f64,
}

impl Spring {
    fn new(value: f64, target: f64) -> Self {
        Self {
            value,
            velocity: 0.0,
            target,
        }
    }

    fn active(&self) -> bool {
        self.value != self.target || self.velocity != 0.0
    }

    fn advance(&mut self, dt: f64) {
        if !self.active() {
            return;
        }
        let omega = 24.0;
        let displacement = self.value - self.target;
        let c = self.velocity + omega * displacement;
        let decay = (-omega * dt).exp();
        self.value = self.target + (displacement + c * dt) * decay;
        self.velocity = (self.velocity - omega * c * dt) * decay;
        // Finish exactly: no invisible tail keeping the event loop awake.
        if (self.value - self.target).abs() < 0.0001 && self.velocity.abs() < 0.002 {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Visual {
    pub scale: f64,
    pub alpha: f32,
    /// Logical pivot and translation, shared by every part of a window.
    pub center: (f64, f64),
    pub offset: (f64, f64),
}

impl Default for Visual {
    fn default() -> Self {
        Self {
            scale: 1.0,
            alpha: 1.0,
            center: (0.0, 0.0),
            offset: (0.0, 0.0),
        }
    }
}

impl Visual {
    fn appearing(progress: f64, center: (f64, f64), minimum: f64) -> Self {
        Self {
            scale: minimum + (1.0 - minimum) * progress,
            alpha: progress.clamp(0.0, 1.0) as f32,
            center,
            offset: (0.0, 0.0),
        }
    }

    pub fn identity(&self) -> bool {
        self.scale == 1.0 && self.alpha == 1.0 && self.offset == (0.0, 0.0)
    }
}

struct WindowMotion {
    spring: Spring,
    minimized: bool,
    dock: Option<R>,
}

/// Switching desktops: windows of `from` leave, windows of `to` come in from the side
struct Slide {
    spring: Spring,
    fresh: bool,
    from: u32,
    to: u32,
    /// +1 when the new desktop is to the right
    dir: f64,
    width: f64,
}

/// Mission Control: 0 = windows where they are, 1 = spread into their overview rectangles
struct MissionMotion {
    spring: Spring,
    fresh: bool,
    targets: HashMap<u64, R>,
}

#[derive(Default)]
pub struct Animations {
    disabled: bool,
    /// Auto-hide Dock: 0 shown, 1 below the screen
    dock: Option<Spring>,
    slide: Option<Slide>,
    mission: Option<MissionMotion>,
    windows: HashMap<u64, WindowMotion>,
    overlay_key: Option<OverlayKind>,
    overlay: Option<Spring>,
    launchpad: Option<Spring>,
    last_frame: Option<Instant>,
}

impl Animations {
    pub fn enabled(&self) -> bool {
        !self.disabled
    }
    pub fn set_enabled(&mut self, enabled: bool, windows: &[ManagedWindow], shell: &MacShell) {
        self.disabled = !enabled;
        self.windows.clear();
        self.slide = None;
        self.mission = None;
        self.overlay = None;
        self.launchpad = None;
        self.last_frame = None;
        self.overlay_key = shell.animation_overlay().map(|(k, _)| k);
        if enabled {
            for w in windows.iter().filter(|w| w.mapped) {
                let value = if w.minimized { 0.0 } else { 1.0 };
                self.windows.insert(
                    w.id,
                    WindowMotion {
                        spring: Spring::new(value, value),
                        minimized: w.minimized,
                        dock: w.minimized.then(|| shell.minimized_target(w.id)),
                    },
                );
            }
            self.overlay = self.overlay_key.map(|_| Spring::new(1.0, 1.0));
            self.launchpad = shell.launchpad.as_ref().map(|_| Spring::new(1.0, 1.0));
        }
    }
    pub fn active(&self) -> bool {
        self.last_frame.is_some()
    }

    /// Opening keeps (or starts) the spring towards 1 with new rectangles; closing turns it
    /// back to 0 and keeps them, so windows fly back to their places
    pub fn set_mission(&mut self, open: bool, targets: &[(u64, R)]) {
        let start = if self.disabled { 1.0 } else { 0.0 };
        let m = self.mission.get_or_insert_with(|| MissionMotion { spring: Spring::new(start, 1.0), fresh: true, targets: HashMap::new() });
        if open {
            m.targets = targets.iter().copied().collect();
            m.spring.target = 1.0;
        } else {
            m.spring.target = 0.0;
        }
        if self.disabled {
            m.spring = Spring::new(m.spring.target, m.spring.target);
            if !open {
                self.mission = None;
            }
        }
    }

    pub fn move_mission_target(&mut self, id: u64, r: R) {
        if let Some(m) = self.mission.as_mut() {
            m.targets.insert(id, r);
        }
    }

    /// Still on screen: open, or flying back
    pub fn mission_visible(&self) -> bool {
        self.mission.is_some()
    }

    pub fn set_dock_hidden(&mut self, hidden: bool) {
        let target = if hidden { 1.0 } else { 0.0 };
        if self.disabled {
            self.dock = Some(Spring::new(target, target));
            return;
        }
        let s = self.dock.get_or_insert_with(|| Spring::new(1.0 - target, target));
        s.target = target;
    }

    /// How far the Dock is pushed below the screen (0…1)
    pub fn dock_hidden(&self) -> f64 {
        self.dock.as_ref().map_or(0.0, |s| s.value)
    }

    /// This desktop is sliding in or out right now
    pub fn in_slide(&self, space: u32) -> bool {
        self.slide.as_ref().is_some_and(|s| s.from == space || s.to == space)
    }

    pub fn mission_progress(&self) -> f64 {
        self.mission.as_ref().map_or(0.0, |m| m.spring.value)
    }

    pub fn slide_spaces(&mut self, from: u32, to: u32, width: f64) {
        if self.disabled || from == to {
            self.slide = None;
            return;
        }
        let dir = if to > from { 1.0 } else { -1.0 };
        self.slide = Some(Slide { spring: Spring::new(0.0, 1.0), fresh: true, from, to, dir, width });
    }

    /// Called only for frames requested by events or a still-running spring.
    pub fn advance(&mut self, windows: &[ManagedWindow], shell: &MacShell, now: Instant) -> bool {
        if self.disabled {
            return false;
        }
        let dt = self
            .last_frame
            .map_or(0.0, |last| now.duration_since(last).as_secs_f64());
        self.windows
            .retain(|id, _| windows.iter().any(|w| w.id == *id && w.mapped));
        for w in windows.iter().filter(|w| w.mapped) {
            let is_new = !self.windows.contains_key(&w.id);
            let motion = self.windows.entry(w.id).or_insert_with(|| WindowMotion {
                spring: Spring::new(0.0, if w.minimized { 0.0 } else { 1.0 }),
                minimized: w.minimized,
                dock: w.minimized.then(|| shell.minimized_target(w.id)),
            });
            if motion.minimized != w.minimized {
                if w.minimized || motion.dock.is_none() {
                    motion.dock = Some(shell.minimized_target(w.id));
                }
                motion.minimized = w.minimized;
                motion.spring.target = if w.minimized { 0.0 } else { 1.0 };
            }
            // Newly created motions start on this frame, not at the previous idle frame.
            // Existing motions continue with their real elapsed time, including after VT changes.
            if !is_new {
                motion.spring.advance(dt);
            }
        }
        let key = shell.animation_overlay().map(|(key, _)| key);
        if key != self.overlay_key {
            self.overlay_key = key;
            self.overlay = key.map(|_| Spring::new(0.0, 1.0));
        } else if let Some(s) = self.overlay.as_mut() {
            s.advance(dt);
        }
        if shell.launchpad.is_some() {
            match self.launchpad.as_mut() {
                Some(s) => s.advance(dt),
                None => self.launchpad = Some(Spring::new(0.0, 1.0)),
            }
        } else {
            self.launchpad = None;
        }
        if let Some(slide) = self.slide.as_mut() {
            // Starts on this frame, not at the previous idle one
            if !std::mem::take(&mut slide.fresh) {
                slide.spring.advance(dt);
            }
            if !slide.spring.active() {
                self.slide = None;
            }
        }
        if let Some(d) = self.dock.as_mut() {
            d.advance(dt);
        }
        if let Some(m) = self.mission.as_mut() {
            if !std::mem::take(&mut m.fresh) {
                m.spring.advance(dt);
            }
            if m.spring.target == 0.0 && !m.spring.active() {
                self.mission = None;
            }
        }
        let active = self.slide.is_some()
            || self.dock.as_ref().is_some_and(Spring::active)
            || self.mission.as_ref().is_some_and(|m| m.spring.active())
            || self.windows.values().any(|w| w.spring.active())
            || self.overlay.as_ref().is_some_and(Spring::active)
            || self.launchpad.as_ref().is_some_and(Spring::active);
        self.last_frame = active.then_some(now);
        active
    }

    pub fn window(&self, w: &ManagedWindow) -> Visual {
        let mut visual = self.window_motion(w);
        if let Some((m, r)) = self.mission.as_ref().and_then(|m| Some((m, m.targets.get(&w.id)?))) {
            let p = m.spring.value;
            let (cx, cy) = (w.x as f64 + w.width as f64 * 0.5, w.y as f64 + w.total_height() as f64 * 0.5);
            let k = r.w as f64 / w.width.max(1) as f64;
            visual.center = (cx, cy);
            visual.scale *= 1.0 + (k - 1.0) * p;
            visual.offset.0 += ((r.x + r.w * 0.5) as f64 - cx) * p;
            visual.offset.1 += ((r.y + r.h * 0.5) as f64 - cy) * p;
        }
        if let Some(s) = self.slide.as_ref() {
            let p = s.spring.value;
            if w.space == s.from {
                visual.offset.0 -= s.dir * p * s.width;
            } else if w.space == s.to {
                visual.offset.0 += s.dir * (1.0 - p) * s.width;
            }
        }
        visual
    }

    fn window_motion(&self, w: &ManagedWindow) -> Visual {
        if self.disabled {
            return Visual::default();
        }
        let Some(motion) = self.windows.get(&w.id) else {
            return Visual::default();
        };
        let center = (
            w.x as f64 + w.width as f64 * 0.5,
            w.y as f64 + w.total_height() as f64 * 0.5,
        );
        let progress = motion.spring.value;
        let Some(dock) = motion.dock else {
            return Visual::appearing(progress, center, 0.92);
        };
        let minimum =
            (dock.w as f64 / w.width.max(w.total_height()).max(1) as f64).clamp(0.01, 1.0);
        let mut visual = Visual::appearing(progress, center, minimum);
        visual.offset = (
            (dock.x as f64 + dock.w as f64 * 0.5 - center.0) * (1.0 - progress),
            (dock.y as f64 + dock.h as f64 * 0.5 - center.1) * (1.0 - progress),
        );
        visual
    }

    pub fn draws_window(&self, w: &ManagedWindow) -> bool {
        // The desktop being left stays drawn while it slides out
        let on_screen = !w.off_space || self.slide.as_ref().is_some_and(|s| w.space == s.from || w.space == s.to);
        w.mapped
            && on_screen
            && (!w.minimized
                || (!self.disabled && self.windows.get(&w.id).is_some_and(|m| m.spring.active())))
    }

    pub fn overlay(&self, shell: &MacShell) -> Visual {
        if self.disabled {
            return Visual::default();
        }
        let Some((_, rect)) = shell.animation_overlay() else {
            return Visual::default();
        };
        Visual::appearing(
            self.overlay.as_ref().map_or(1.0, |s| s.value),
            (
                (rect.x + rect.w * 0.5) as f64,
                (rect.y + rect.h * 0.5) as f64,
            ),
            0.96,
        )
    }

    pub fn launchpad(&self, shell: &MacShell) -> Visual {
        if self.disabled {
            return Visual::default();
        }
        Visual::appearing(
            self.launchpad.as_ref().map_or(1.0, |s| s.value),
            (shell.screen_w as f64 * 0.5, shell.screen_h as f64 * 0.5),
            0.96,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_is_refresh_rate_independent_and_stops() {
        let mut a = Spring::new(0.0, 1.0);
        let mut b = a;
        for _ in 0..30 {
            a.advance(1.0 / 60.0);
        }
        for _ in 0..72 {
            b.advance(1.0 / 144.0);
        }
        assert!((a.value - b.value).abs() < 1e-12);
        for _ in 0..144 {
            b.advance(1.0 / 144.0);
        }
        assert_eq!(b.value, 1.0);
        assert!(!b.active());
    }

    #[test]
    fn disabling_mid_flight_snaps_and_reenabling_does_not_replay_existing_windows() {
        use crate::apps::BuiltinWindowKind;
        let shell = MacShell::new(1440, 900);
        let mut windows = vec![ManagedWindow::new_builtin(
            1,
            BuiltinWindowKind::SystemSettings,
            40,
            40,
        )];
        let mut animations = Animations::default();
        let now = Instant::now();
        assert!(animations.advance(&windows, &shell, now));
        animations.set_enabled(false, &windows, &shell);
        assert!(!animations.enabled());
        assert!(!animations.active());
        assert!(animations.window(&windows[0]).identity());
        assert!(!animations.advance(&windows, &shell, now));
        windows[0].minimized = true;
        assert!(!animations.draws_window(&windows[0]));
        windows[0].minimized = false;
        animations.set_enabled(true, &windows, &shell);
        assert!(!animations.advance(&windows, &shell, now));
        assert!(animations.window(&windows[0]).identity());
        windows.push(ManagedWindow::new_builtin(
            2,
            BuiltinWindowKind::QuickNotes,
            40,
            40,
        ));
        assert!(animations.advance(&windows, &shell, now));
        assert!(!animations.window(&windows[1]).identity());
    }

    #[test]
    fn reversal_preserves_position_and_velocity() {
        let mut s = Spring::new(1.0, 0.0);
        s.advance(0.05);
        let before = (s.value, s.velocity);
        s.target = 1.0;
        assert_eq!((s.value, s.velocity), before);
        s.advance(1.0 / 144.0);
        assert!(s.value.is_finite());
        s.advance(5.0);
        assert_eq!(s.value, 1.0);
        assert!(!s.active());
    }
    #[test]
    fn window_lifecycle_finishes_and_minimized_windows_never_take_input() {
        use crate::apps::BuiltinWindowKind;
        use std::time::Duration;
        let shell = MacShell::new(1440, 900);
        let mut windows = vec![ManagedWindow::new_builtin(
            1,
            BuiltinWindowKind::AboutFeather,
            100,
            100,
        )];
        let mut animations = Animations::default();
        let start = Instant::now();
        assert!(animations.advance(&windows, &shell, start));
        assert_eq!(animations.window(&windows[0]).scale, 0.92);
        assert!(animations.advance(&windows, &shell, start + Duration::from_millis(16)));
        assert!(animations.window(&windows[0]).alpha > 0.0);
        assert!(!animations.advance(&windows, &shell, start + Duration::from_secs(2)));
        assert!(animations.window(&windows[0]).identity());
        windows[0].minimized = true;
        assert!(animations.advance(&windows, &shell, start + Duration::from_secs(3)));
        assert!(animations.draws_window(&windows[0]));
        assert!(!windows[0].is_visible());
        assert!(!animations.advance(&windows, &shell, start + Duration::from_secs(5)));
        assert!(!animations.draws_window(&windows[0]));
        let v = animations.window(&windows[0]);
        let dock = shell.minimized_target(1);
        assert!((v.center.0 + v.offset.0 - (dock.x + dock.w * 0.5) as f64).abs() < 0.001);
        windows[0].minimized = false;
        assert!(animations.advance(&windows, &shell, start + Duration::from_secs(6)));
        assert_eq!(animations.window(&windows[0]).alpha, 0.0);
        assert!(!animations.advance(&windows, &shell, start + Duration::from_secs(8)));
        assert!(animations.window(&windows[0]).identity());
        windows.clear();
        assert!(!animations.advance(&windows, &shell, start + Duration::from_secs(9)));
        assert!(animations.windows.is_empty());
    }
    #[test]
    fn overlay_content_updates_keep_motion_and_closing_removes_it() {
        use std::time::Duration;
        let mut shell = MacShell::new(1440, 900);
        let mut animations = Animations::default();
        let start = Instant::now();
        shell.spotlight_open = true;
        assert!(animations.advance(&[], &shell, start));
        assert_eq!(animations.overlay(&shell).alpha, 0.0);
        animations.advance(&[], &shell, start + Duration::from_millis(100));
        let alpha = animations.overlay(&shell).alpha;
        shell.spotlight_query = "test".into();
        animations.advance(&[], &shell, start + Duration::from_millis(116));
        assert!(animations.overlay(&shell).alpha > alpha);
        shell.spotlight_open = false;
        assert!(!animations.advance(&[], &shell, start + Duration::from_millis(120)));
        assert!(animations.last_frame.is_none());
        assert!(animations.overlay.is_none());
        shell.control_center_open = true;
        assert!(animations.advance(&[], &shell, start + Duration::from_secs(10)));
        assert_eq!(animations.overlay(&shell).alpha, 0.0);
    }
}
