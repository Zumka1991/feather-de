//! A single cancellable timer exists only while a field or volume key is held.
//! Repeats go straight to the editor: they never fake wl_keyboard events or update XKB.
use crate::{apps::BuiltinWindowKind, input::KeyAction, window::WindowBackend, FeatherCompositor};
use smithay::{
    input::keyboard::{keysyms as k, Keysym, ModifiersState},
    reexports::calloop::{
        timer::{TimeoutAction, Timer},
        RegistrationToken,
    },
};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Search,
    Applications,
    Settings(u64, crate::system_settings::Page, bool),
    Wifi,
    Audio,
}
#[derive(Clone, Copy)]
pub(crate) struct Candidate {
    target: Target,
    sym: Keysym,
    latin: Keysym,
    ctrl: bool,
    shift: bool,
}
#[derive(Default)]
pub struct Repeat {
    token: Option<RegistrationToken>,
}
fn repeatable(sym: Keysym, mods: &ModifiersState) -> bool {
    if mods.logo || mods.alt {
        return false;
    }
    if mods.ctrl {
        return matches!(
            sym.raw(),
            k::KEY_Left | k::KEY_Right | k::KEY_BackSpace | k::KEY_Delete
        );
    }
    matches!(
        sym.raw(),
        k::KEY_Left | k::KEY_Right | k::KEY_BackSpace | k::KEY_Delete | k::KEY_Up | k::KEY_Down
    ) || sym.key_char().is_some_and(|c| !c.is_control())
}
impl FeatherCompositor {
    fn repeat_target(&self) -> Option<Target> {
        if self.lock.active
            || self.auth.is_open()
            || self.capturing_shortcut()
            || self.shell.switcher.is_some()
        {
            return None;
        }
        if self.shell.control_center_open
            && self.shell.cc.page == crate::control_panel::Page::Password
        {
            return Some(Target::Wifi);
        }
        if self.shell.open_menu.is_some() {
            return None;
        }
        if self.shell.launchpad.is_some() {
            return Some(Target::Applications);
        }
        if self.shell.spotlight_open {
            return Some(Target::Search);
        }
        self.windows
            .iter()
            .rev()
            .find(|w| w.is_visible())
            .and_then(|w| {
                (self.system_settings.editing()
                    && matches!(
                        w.backend,
                        WindowBackend::Builtin(BuiltinWindowKind::SystemSettings)
                    ))
                .then_some(Target::Settings(
                    w.id,
                    self.system_settings.page,
                    self.system_settings.zone_picker,
                ))
            })
    }
    pub(crate) fn repeat_candidate(
        &self,
        sym: Keysym,
        latin: Keysym,
        mods: &ModifiersState,
    ) -> Option<Candidate> {
        if !self.lock.active
            && crate::controls::AudioKey::from_keysym(sym.raw())
                .is_some_and(|k| k != crate::controls::AudioKey::Mute)
        {
            return Some(Candidate {
                target: Target::Audio,
                sym,
                latin,
                ctrl: false,
                shift: false,
            });
        }
        if !repeatable(sym, mods) {
            return None;
        }
        let target = self.repeat_target()?;
        if matches!(sym.raw(), k::KEY_Up | k::KEY_Down) && target != Target::Search {
            return None;
        }
        // Empty Launchpad arrows turn pages rather than editing the field.
        if target == Target::Applications
            && self
                .shell
                .launchpad
                .as_ref()
                .is_some_and(|l| l.input.is_empty())
            && matches!(sym.raw(), k::KEY_Left | k::KEY_Right)
        {
            return None;
        }
        Some(Candidate {
            target,
            sym,
            latin,
            ctrl: mods.ctrl,
            shift: mods.shift,
        })
    }
    fn repeat_still_valid(&self, target: Target) -> bool {
        if target == Target::Audio {
            !self.lock.active
        } else {
            self.repeat_target() == Some(target)
        }
    }
    pub fn cancel_key_repeat(&mut self) {
        if let Some(token) = self.key_repeat.token.take() {
            self.loop_handle.remove(token);
        }
    }
    pub(crate) fn arm_key_repeat(&mut self, key: Candidate) {
        if !self.repeat_still_valid(key.target) {
            return;
        }
        let delay = Duration::from_millis(self.keyboard_config.repeat_delay as u64);
        let interval = if key.target == Target::Audio {
            Duration::from_millis(80)
        } else {
            Duration::from_secs_f64(1.0 / self.keyboard_config.repeat_rate.max(1) as f64)
        };
        if let Ok(token) =
            self.loop_handle
                .insert_source(Timer::from_duration(delay), move |_, _, st| {
                    if !st.repeat_still_valid(key.target) {
                        st.key_repeat.token = None;
                        return TimeoutAction::Drop;
                    }
                    // Typing while held counts as activity, just like physical key events.
                    if st.activity() {
                        st.key_repeat.token = None;
                        return TimeoutAction::Drop;
                    }
                    if key.target == Target::Audio {
                        if let Some(key) = crate::controls::AudioKey::from_keysym(key.sym.raw()) {
                            st.handle_audio_key(key);
                        }
                    } else if key.target == Target::Wifi {
                        let _ =
                            st.shell
                                .cc
                                .key(key.sym.raw(), key.sym.key_char(), key.ctrl, key.shift);
                        st.shell.overlay_dirty = true;
                    } else {
                        let mut action: Option<KeyAction> = None;
                        st.edit_builtin_key(key.sym, key.latin, key.ctrl, key.shift, &mut action);
                    }
                    st.needs_redraw = true;
                    TimeoutAction::ToDuration(interval)
                })
        {
            self.key_repeat.token = Some(token);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeat_only_edits_never_submits_or_runs_shortcuts() {
        let plain = ModifiersState::default();
        for sym in [
            k::KEY_a,
            k::KEY_Cyrillic_ya,
            k::KEY_Left,
            k::KEY_BackSpace,
            k::KEY_Delete,
        ] {
            assert!(repeatable(Keysym::new(sym), &plain));
        }
        for sym in [
            k::KEY_Return,
            k::KEY_Escape,
            k::KEY_Tab,
            k::KEY_F1,
            k::KEY_Shift_L,
        ] {
            assert!(!repeatable(Keysym::new(sym), &plain));
        }
        assert!(!repeatable(
            Keysym::new(k::KEY_a),
            &ModifiersState {
                ctrl: true,
                ..plain
            }
        ));
        assert!(!repeatable(
            Keysym::new(k::KEY_a),
            &ModifiersState {
                logo: true,
                ..plain
            }
        ));
        assert!(repeatable(
            Keysym::new(k::KEY_Left),
            &ModifiersState {
                ctrl: true,
                shift: true,
                ..plain
            }
        ));
    }
}
