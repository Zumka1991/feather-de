//! Larvis's desktop in the main session. Larvis runs a nested Feather (`--larvis-desktop`)
//! for its own windows; its host window is not a task of the user but a desktop of its own:
//! it is left out of the Dock, Alt+Tab, Mission Control, the desktop count and the API's
//! window list. The Larvis item in the menu bar (and Cmd+U) switches to it and back to the
//! user's desktop, like a desktop switch.
use crate::FeatherCompositor;

/// Desktop index of Larvis's window: outside the user's desktops `0..count`
pub const LARVIS_SPACE: u32 = u32::MAX;

impl FeatherCompositor {
    /// The mapped host window of Larvis's nested desktop
    pub fn larvis_window(&self) -> Option<usize> {
        self.windows.iter().position(|w| w.larvis && w.mapped)
    }

    /// A newly mapped window; true when it is Larvis's desktop and was adopted as such.
    /// Only the main session hosts it: the nested desktop itself has no Larvis desktop.
    pub fn adopt_larvis_window(&mut self, idx: usize) -> bool {
        if self.larvis_desktop || self.windows[idx].app_id != crate::LARVIS_DESKTOP_ID {
            return false;
        }
        // One desktop at a time: an older one (a restarted Larvis) stops being special
        for w in self.windows.iter_mut().filter(|w| w.larvis) {
            w.larvis = false;
        }
        let shown = self.larvis_shown;
        let w = &mut self.windows[idx];
        w.larvis = true;
        w.space = LARVIS_SPACE;
        w.minimized = false;
        w.off_space = !shown;
        // The whole primary screen, so that its own menu bar sits under ours
        let screen = self.primary_monitor().map(|m| m.info.rect).unwrap_or_else(|| self.monitor_at(0.0, 0.0));
        self.set_window_fullscreen(idx, true, screen);
        self.sync_larvis_desktop();
        true
    }

    /// Shows Larvis's desktop or returns to the user's desktop; false when there is none
    pub fn set_larvis_shown(&mut self, shown: bool) -> bool {
        let available = self.larvis_window().is_some();
        let shown = shown && available;
        if shown != self.larvis_shown {
            self.larvis_shown = shown;
            if shown {
                self.shell.switcher = None;
                self.shell.launchpad = None;
                if self.mission_open() {
                    self.close_mission(None);
                }
            }
            self.shell.close_menus();
            self.apply_space_visibility();
            self.unfocus_desktop();
            self.dock_model_dirty = true;
            self.pointer_focus_dirty = true;
            self.refresh_focus();
            self.needs_redraw = true;
        }
        self.sync_larvis_desktop();
        available
    }

    pub fn toggle_larvis_desktop(&mut self) {
        self.set_larvis_shown(!self.larvis_shown);
    }

    /// Back to the user's desktop: before anything of the user comes forward
    pub fn leave_larvis_desktop(&mut self) {
        if self.larvis_shown {
            self.set_larvis_shown(false);
        }
    }

    /// After Larvis's window appeared or went away: the menu bar item follows it, and the
    /// user is never left on a desktop that no longer exists
    pub fn sync_larvis_desktop(&mut self) {
        let available = self.larvis_window().is_some();
        if !available && self.larvis_shown {
            self.larvis_shown = false;
            self.apply_space_visibility();
            self.dock_model_dirty = true;
            self.pointer_focus_dirty = true;
            self.refresh_focus();
            self.needs_redraw = true;
        }
        self.shell.set_larvis(available.then_some(self.larvis_shown));
    }
}
