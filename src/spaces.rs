//! Desktops (Spaces). Every window belongs to one; windows of other desktops are hidden
//! (`off_space`) but stay in Alt+Tab and the Dock, which bring their desktop forward.
//! Desktops grow on demand: moving right from a non-empty last desktop adds one, and an
//! empty desktop at the end disappears once it is left. Switching slides the two
//! desktops' windows with the shared spring (GPU offsets, no repaint of client buffers).
use crate::FeatherCompositor;

pub const MAX_SPACES: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spaces {
    pub current: u32,
    pub count: u32,
}

impl Default for Spaces {
    fn default() -> Self {
        Self { current: 0, count: 1 }
    }
}

/// Where a step of `delta` leads and how many desktops there are then; None stays put
pub fn step(s: Spaces, delta: i32, current_has_windows: bool) -> Option<Spaces> {
    let target = s.current as i64 + delta as i64;
    if target < 0 || delta == 0 {
        return None;
    }
    let target = target as u32;
    if target < s.count {
        return Some(Spaces { current: target, count: s.count });
    }
    // Past the last one: a new desktop, unless this one is already empty
    (target == s.count && current_has_windows && s.count < MAX_SPACES).then_some(Spaces { current: target, count: s.count + 1 })
}

/// Desktops after removing `space`; the current one follows its content
pub fn removed(s: Spaces, space: u32) -> Option<Spaces> {
    if s.count <= 1 || space >= s.count {
        return None;
    }
    let current = if s.current > space { s.current - 1 } else if s.current == space { space.saturating_sub(1) } else { s.current };
    Some(Spaces { current, count: s.count - 1 })
}

/// Drops empty desktops at the end, never the current one or the first
pub fn prune(mut s: Spaces, has_windows: impl Fn(u32) -> bool) -> Spaces {
    while s.count > 1 && s.count - 1 > s.current && !has_windows(s.count - 1) {
        s.count -= 1;
    }
    s
}

impl FeatherCompositor {
    fn space_has_windows(&self, space: u32) -> bool {
        self.windows.iter().any(|w| w.mapped && w.space == space)
    }

    pub fn apply_space_visibility(&mut self) {
        let current = self.spaces.current;
        for w in &mut self.windows {
            w.off_space = w.space != current;
        }
    }

    pub fn switch_space(&mut self, target: u32) {
        if target == self.spaces.current || target >= self.spaces.count {
            return;
        }
        let from = self.spaces.current;
        self.spaces.current = target;
        self.spaces = prune(self.spaces, |s| self.space_has_windows(s));
        self.apply_space_visibility();
        self.animations.slide_spaces(from, target, self.shell.screen_w as f64);
        self.shell.close_menus();
        self.unfocus_desktop();
        self.dock_model_dirty = true;
        self.pointer_focus_dirty = true;
        self.refresh_focus();
        self.needs_redraw = true;
    }

    /// Ctrl+Alt+←/→; with `carry` (Shift) the front window comes along
    pub fn space_step(&mut self, delta: i32, carry: bool) {
        let front = self.windows.iter().rposition(|w| w.is_visible());
        let has = self.space_has_windows(self.spaces.current);
        let Some(next) = step(self.spaces, delta, has) else { return };
        self.spaces.count = next.count;
        if carry {
            if let Some(i) = front {
                self.windows[i].space = next.current;
            }
        }
        self.switch_space(next.current);
    }

    /// Debug check helper: a step left that must change nothing
    pub fn space_step_noop_left(&mut self) -> bool {
        let before = self.spaces;
        self.space_step(-1, false);
        self.spaces == before
    }

    /// Mission Control's × on a desktop: its windows join the desktop on its left
    /// (the first one's go to the next), later desktops shift down by one
    pub fn remove_space(&mut self, space: u32) {
        let Some(next) = removed(self.spaces, space) else { return };
        let home = space.saturating_sub(1);
        for w in &mut self.windows {
            if w.space == space {
                w.space = home;
            } else if w.space > space {
                w.space -= 1;
            }
        }
        self.spaces = next;
        self.apply_space_visibility();
        self.dock_model_dirty = true;
        self.pointer_focus_dirty = true;
        self.refresh_focus();
        self.needs_redraw = true;
    }

    /// A window about to be shown: its desktop comes forward first
    pub fn reveal_window_space(&mut self, id: u64) {
        if let Some(space) = self.windows.iter().find(|w| w.id == id && w.mapped && !w.minimized).map(|w| w.space) {
            self.switch_space(space);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktops_grow_on_demand_and_empty_ones_at_the_end_go_away() {
        let one = Spaces::default();
        assert_eq!(step(one, -1, true), None, "nothing left of the first");
        assert_eq!(step(one, 1, false), None, "an empty desktop does not spawn another");
        let two = step(one, 1, true).unwrap();
        assert_eq!(two, Spaces { current: 1, count: 2 });
        assert_eq!(step(two, -1, false), Some(Spaces { current: 0, count: 2 }));
        // Back on the first: the empty second one disappears, a non-empty one stays
        let back = Spaces { current: 0, count: 3 };
        assert_eq!(prune(back, |s| s == 1), Spaces { current: 0, count: 2 });
        assert_eq!(prune(back, |s| s == 2), back);
        assert_eq!(prune(Spaces { current: 2, count: 3 }, |_| false).count, 3, "never the current one");
        assert_eq!(removed(Spaces { current: 2, count: 3 }, 1), Some(Spaces { current: 1, count: 2 }), "later desktops shift");
        assert_eq!(removed(Spaces { current: 1, count: 3 }, 1), Some(Spaces { current: 0, count: 2 }), "current one removed: its left neighbour");
        assert_eq!(removed(Spaces { current: 0, count: 3 }, 0), Some(Spaces { current: 0, count: 2 }));
        assert_eq!(removed(Spaces::default(), 0), None, "the last desktop stays");
        let full = Spaces { current: MAX_SPACES - 1, count: MAX_SPACES };
        assert_eq!(step(full, 1, true), None);
    }
}
