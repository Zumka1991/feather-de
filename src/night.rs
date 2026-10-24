//! Night Shift: warmer colours by hand or on a daily schedule, through the CRTC gamma
//! table of the direct DRM session. The existing one-second housekeeping tick moves the
//! colour temperature towards its target in small steps (a ~10 s fade like macOS) and does
//! nothing once it is reached: no timer of its own, no work when idle.

/// Neutral white point
pub const NEUTRAL: u32 = 6500;
/// Kelvin per one-second step of a fade
const STEP: u32 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preferences {
    /// Turned on by hand ("Включить до завтра" on macOS; here until turned off)
    pub manual: bool,
    /// Daily window in minutes after midnight (from, to)
    pub schedule: Option<(u16, u16)>,
    /// Target colour temperature while active
    pub kelvin: u32,
}

impl Default for Preferences {
    fn default() -> Self {
        Self { manual: false, schedule: None, kelvin: 4000 }
    }
}

/// Offered warmth, cycled by a click in Settings
pub const WARMTH: [(u32, &str); 4] = [(5000, "Слегка тёплые"), (4000, "Тёплые"), (3400, "Очень тёплые"), (2800, "Максимально тёплые")];

/// Offered schedules
pub const WINDOWS: [(u16, u16); 4] = [(22 * 60, 7 * 60), (21 * 60, 7 * 60), (20 * 60, 6 * 60), (23 * 60, 8 * 60)];

pub fn warmth_label(kelvin: u32) -> String {
    WARMTH.iter().find(|(k, _)| *k == kelvin).map_or_else(|| format!("{kelvin} K"), |(k, l)| format!("{l} · {k} K"))
}

#[derive(Clone, Copy, Debug)]
pub struct NightShift {
    pub prefs: Preferences,
    /// Temperature on screen now
    pub current: u32,
    in_window: bool,
    /// Turned off by hand inside the window: stays off until the window ends
    skipped: bool,
}

impl NightShift {
    pub fn new(prefs: Preferences) -> Self {
        Self { prefs, current: NEUTRAL, in_window: false, skipped: false }
    }

    pub fn active(&self) -> bool {
        self.prefs.manual || (self.in_window && !self.skipped)
    }

    pub fn target(&self) -> u32 {
        if self.active() { self.prefs.kelvin.clamp(1900, NEUTRAL) } else { NEUTRAL }
    }

    /// The switch: off inside the schedule ends tonight's window early
    pub fn toggle(&mut self) {
        if self.active() {
            self.prefs.manual = false;
            self.skipped = self.in_window;
        } else {
            self.prefs.manual = true;
        }
    }

    /// One housekeeping second at `minute`; true when the screen colour must change
    pub fn tick(&mut self, minute: u16) -> bool {
        let inside = self.prefs.schedule.is_some_and(|(f, t)| crate::shell::in_window(minute, f, t));
        if inside != self.in_window {
            self.in_window = inside;
            if !inside {
                self.skipped = false;
            }
        }
        let target = self.target();
        if self.current == target {
            return false;
        }
        self.current = if self.current < target { (self.current + STEP).min(target) } else { self.current.saturating_sub(STEP).max(target) };
        true
    }

    /// Settings changed by the user: show the result at once, no fade
    pub fn jump(&mut self) -> bool {
        let target = self.target();
        std::mem::replace(&mut self.current, target) != target
    }
}

/// Relative RGB of a black body at `kelvin`, white (1, 1, 1) at 6500 K. Tanner Helland's fit
/// of the CIE data, normalised; accurate enough for a warm screen tint.
pub fn kelvin_rgb(kelvin: u32) -> [f64; 3] {
    let raw = |k: f64| -> [f64; 3] {
        let t = k / 100.0;
        let r = if t <= 66.0 { 255.0 } else { 329.698_727_446 * (t - 60.0).powf(-0.133_204_759_2) };
        let g = if t <= 66.0 { 99.470_802_586_1 * t.ln() - 161.119_568_166_1 } else { 288.122_169_528_3 * (t - 60.0).powf(-0.075_514_849_2) };
        let b = if t >= 66.0 { 255.0 } else if t <= 19.0 { 0.0 } else { 138.517_731_223_1 * (t - 10.0).ln() - 305.044_792_730_7 };
        [r.clamp(0.0, 255.0), g.clamp(0.0, 255.0), b.clamp(0.0, 255.0)]
    };
    let (c, white) = (raw(kelvin as f64), raw(NEUTRAL as f64));
    [0, 1, 2].map(|i| (c[i] / white[i]).clamp(0.0, 1.0))
}

/// Gamma ramps of `size` entries for the given multipliers
pub fn ramps(size: usize, rgb: [f64; 3]) -> [Vec<u16>; 3] {
    let n = size.max(2) - 1;
    rgb.map(|m| (0..size).map(|i| ((i as f64 / n as f64) * m * 65535.0).round() as u16).collect())
}

impl crate::FeatherCompositor {
    /// Writes the current temperature to the CRTC (DRM only; a nested window cannot)
    pub fn apply_night_shift(&mut self) {
        let kelvin = self.night.current;
        if let crate::backend::Backend::Drm(d) = &mut self.backend {
            if !d.active {
                return;
            }
            use smithay::reexports::drm::control::Device;
            for head in &d.heads {
            let size = d.fd.get_crtc(head.crtc).map(|c| c.gamma_length() as usize).unwrap_or(0);
            if size < 2 {
                continue;
            }
            let [r, g, b] = ramps(size, kelvin_rgb(kelvin));
            if let Err(e) = d.fd.set_gamma(head.crtc, &r, &g, &b) {
                tracing::warn!("Ночной режим: гамма не применена: {e}");
            }
            }
        }
    }

    /// From the one-second tick
    pub fn night_tick(&mut self) {
        use chrono::Timelike;
        let now = chrono::Local::now();
        if self.night.tick((now.hour() * 60 + now.minute()) as u16) {
            self.apply_night_shift();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperatures_are_white_at_6500_and_warm_below() {
        let w = kelvin_rgb(NEUTRAL);
        assert!(w.iter().all(|c| (c - 1.0).abs() < 1e-9));
        let warm = kelvin_rgb(3400);
        assert!(warm[0] == 1.0 && warm[1] < 1.0 && warm[2] < warm[1], "red stays, blue goes first: {warm:?}");
        let warmer = kelvin_rgb(2800);
        assert!(warmer[2] < warm[2]);
        let [r, _, b] = ramps(256, warm);
        assert_eq!((r[0], r[255]), (0, 65535));
        assert!(b.windows(2).all(|p| p[0] <= p[1]) && b[255] < 65535);
    }

    #[test]
    fn schedule_fades_in_and_out_and_can_be_ended_early() {
        let mut n = NightShift::new(Preferences { manual: false, schedule: Some((22 * 60, 7 * 60)), kelvin: 4000 });
        assert!(!n.tick(12 * 60), "day: nothing to do");
        let mut steps = 0;
        while n.tick(23 * 60) { steps += 1; }
        assert_eq!(n.current, 4000);
        assert!((8..=10).contains(&steps), "a fade of a few seconds, {steps}");
        assert!(!n.tick(23 * 60), "idle once reached");
        n.toggle();
        assert!(!n.active() && !n.prefs.manual);
        while n.tick(1 * 60) {}
        assert_eq!(n.current, NEUTRAL, "off for the rest of the night");
        n.tick(8 * 60);
        n.tick(22 * 60);
        assert!(n.active(), "the next evening applies again");
        n.prefs.kelvin = 2800;
        assert!(n.jump() && n.current == 2800, "settings apply at once");
        let mut manual = NightShift::new(Default::default());
        manual.toggle();
        assert!(manual.active() && manual.target() == 4000);
        assert_eq!(warmth_label(4000), "Тёплые · 4000 K");
    }
}
