//! Hot corners: the pointer hitting a screen corner runs that corner's action. A plain
//! comparison on pointer motion, no timer; a corner fires once and re-arms only after the
//! pointer has moved away, so resting in it does not repeat the action.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HotAction {
    #[default]
    None,
    MissionControl,
    Launchpad,
    Desktop,
    Notifications,
    Spotlight,
    Lock,
}

impl HotAction {
    const ALL: [Self; 7] = [Self::None, Self::MissionControl, Self::Launchpad, Self::Desktop, Self::Notifications, Self::Spotlight, Self::Lock];

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|a| *a == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "Нет действия",
            Self::MissionControl => "Mission Control",
            Self::Launchpad => "Launchpad",
            Self::Desktop => "Рабочий стол",
            Self::Notifications => "Центр уведомлений",
            Self::Spotlight => "Spotlight",
            Self::Lock => "Заблокировать экран",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::MissionControl => "mission",
            Self::Launchpad => "launchpad",
            Self::Desktop => "desktop",
            Self::Notifications => "notifications",
            Self::Spotlight => "spotlight",
            Self::Lock => "lock",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.id() == s)
    }
}

/// Top-left, top-right, bottom-left, bottom-right
pub const CORNER_NAMES: [&str; 4] = ["Левый верхний угол", "Правый верхний угол", "Левый нижний угол", "Правый нижний угол"];

/// Distance (logical px) that counts as being in the corner
const HIT: f64 = 2.0;
/// Distance the pointer must leave the corner before it can fire again
const REARM: f64 = 40.0;

#[derive(Clone, Copy, Debug, Default)]
pub struct HotCorners {
    pub actions: [HotAction; 4],
    /// The corner already fired and the pointer has not left it yet
    fired: Option<usize>,
}

impl HotCorners {
    pub fn new(actions: [HotAction; 4]) -> Self {
        Self { actions, fired: None }
    }

    fn near(x: f64, y: f64, (w, h): (f64, f64), d: f64) -> Option<usize> {
        let left = x <= d;
        let right = x >= w - 1.0 - d;
        let top = y <= d;
        let bottom = y >= h - 1.0 - d;
        match (left, right, top, bottom) {
            (true, _, true, _) => Some(0),
            (_, true, true, _) => Some(1),
            (true, _, _, true) => Some(2),
            (_, true, _, true) => Some(3),
            _ => None,
        }
    }

    /// The action to run for this pointer position, if a corner was just reached
    pub fn motion(&mut self, x: f64, y: f64, screen: (f64, f64)) -> Option<HotAction> {
        if let Some(c) = self.fired {
            if Self::near(x, y, screen, REARM) == Some(c) {
                return None;
            }
            self.fired = None;
        }
        let corner = Self::near(x, y, screen, HIT)?;
        let action = self.actions[corner];
        if action == HotAction::None {
            return None;
        }
        self.fired = Some(corner);
        Some(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_fire_once_and_rearm_after_leaving() {
        let screen = (1440.0, 900.0);
        let mut h = HotCorners::new([HotAction::Launchpad, HotAction::None, HotAction::Lock, HotAction::Notifications]);
        assert_eq!(h.motion(700.0, 400.0, screen), None);
        assert_eq!(h.motion(0.0, 0.0, screen), Some(HotAction::Launchpad));
        assert_eq!(h.motion(1.0, 0.0, screen), None, "resting in the corner");
        assert_eq!(h.motion(20.0, 20.0, screen), None, "still near");
        assert_eq!(h.motion(0.0, 0.0, screen), None);
        assert_eq!(h.motion(300.0, 300.0, screen), None);
        assert_eq!(h.motion(0.0, 0.0, screen), Some(HotAction::Launchpad), "re-armed");
        assert_eq!(h.motion(1439.0, 0.0, screen), None, "no action set");
        assert_eq!(h.motion(1439.0, 899.0, screen), Some(HotAction::Notifications));
        assert_eq!(h.motion(0.0, 899.0, screen), Some(HotAction::Lock), "another corner fires at once");
        assert_eq!(h.motion(720.0, 0.0, screen), None, "an edge is not a corner");
        assert_eq!(HotAction::Lock.next(), HotAction::None);
        assert!(HotAction::ALL.iter().all(|a| HotAction::parse(a.id()) == Some(*a)));
    }
}
