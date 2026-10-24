//! Configurable desktop chords. Matching uses the unshifted Latin keysym, so a binding
//! keeps working in Russian. System recovery, locking and switcher chords stay reserved.
use smithay::input::keyboard::{keysyms as k, ModifiersState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    pub mods: u8,
    pub key: u32,
}
pub const CTRL: u8 = 1;
pub const ALT: u8 = 2;
pub const SHIFT: u8 = 4;
pub const SUPER: u8 = 8;
impl Chord {
    pub fn new(mods: &ModifiersState, key: u32) -> Self {
        Self {
            mods: u8::from(mods.ctrl)
                | u8::from(mods.alt) * ALT
                | u8::from(mods.shift) * SHIFT
                | u8::from(mods.logo) * SUPER,
            key: if (k::KEY_A..=k::KEY_Z).contains(&key) {
                key + 32
            } else {
                key
            },
        }
    }
    pub fn valid(self) -> bool {
        self.mods < 16
            && self.mods & (CTRL | ALT | SUPER) != 0
            && ((0x20..=0x7e).contains(&self.key)
                || matches!(
                    self.key,
                    k::KEY_Return | k::KEY_Left | k::KEY_Right | k::KEY_Up | k::KEY_Down
                )
                || (k::KEY_F1..=k::KEY_F12).contains(&self.key))
            && !self.reserved()
    }
    fn reserved(self) -> bool {
        // Layout switching and fixed shell/recovery keys are never shadowed.
        self.key == k::KEY_space && self.mods & CTRL != 0
            || self.key == k::KEY_Down && self.mods & (CTRL | ALT) == CTRL | ALT
            || self.mods & SUPER != 0 && self.key == k::KEY_l
            || self.mods & (CTRL | ALT) == CTRL | ALT
                && (k::KEY_F1..=k::KEY_F12).contains(&self.key)
            || self.mods & (ALT | SUPER) != 0 && matches!(self.key, k::KEY_Tab | k::KEY_grave)
            || self.mods & (SUPER | SHIFT) == SUPER | SHIFT && self.key == k::KEY_r
            || self.mods & SUPER != 0
                && matches!(self.key, k::KEY_equal | k::KEY_plus | k::KEY_minus)
    }
    pub fn label(self) -> String {
        let mut parts = Vec::new();
        for (bit, name) in [
            (CTRL, "Ctrl"),
            (ALT, "Alt"),
            (SHIFT, "Shift"),
            (SUPER, "Super"),
        ] {
            if self.mods & bit != 0 {
                parts.push(name.to_string());
            }
        }
        parts.push(match self.key {
            k::KEY_space => "Пробел".into(),
            k::KEY_Return => "Enter".into(),
            k::KEY_Left => "←".into(),
            k::KEY_Right => "→".into(),
            k::KEY_Up => "↑".into(),
            k::KEY_Down => "↓".into(),
            v if (k::KEY_F1..=k::KEY_F12).contains(&v) => format!("F{}", v - k::KEY_F1 + 1),
            v => char::from_u32(v)
                .map(|c| c.to_ascii_uppercase().to_string())
                .unwrap_or_default(),
        });
        parts.join(" + ")
    }
}

macro_rules! actions {
    ($( $variant:ident, $id:literal, $label:literal, $mods:expr, $key:expr; )*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Action { $( $variant, )* }
        impl Action {
            pub const ALL: &'static [Self] = &[$( Self::$variant, )*];
            pub fn id(self) -> &'static str { match self { $(Self::$variant => $id,)* } }
            pub fn label(self) -> &'static str { match self { $(Self::$variant => $label,)* } }
            pub fn default_chord(self) -> Chord { match self { $(Self::$variant => Chord {mods: $mods, key: $key},)* } }
            pub fn parse(s: &str) -> Option<Self> { Self::ALL.iter().copied().find(|a| a.id() == s) }
        }
    };
}
actions! {
    Search, "search", "Поиск", SUPER, k::KEY_space;
    Applications, "applications", "Приложения", SUPER, k::KEY_a;
    Terminal, "terminal", "Терминал", SUPER, k::KEY_Return;
    Files, "files", "Файлы", SUPER, k::KEY_e;
    ControlCenter, "control-center", "Пункт управления", SUPER, k::KEY_n;
    Close, "close", "Закрыть окно", SUPER, k::KEY_w;
    Quit, "quit", "Завершить программу", SUPER, k::KEY_q;
    Minimize, "minimize", "Свернуть окно", SUPER, k::KEY_m;
    Maximize, "maximize", "Развернуть окно", SUPER, k::KEY_f;
    Hide, "hide", "Скрыть программу", SUPER, k::KEY_h;
    HideOthers, "hide-others", "Скрыть остальные", SUPER | ALT, k::KEY_h;
    Mission, "mission", "Обзор окон и столов", CTRL | ALT, k::KEY_Up;
    PreviousSpace, "previous-space", "Предыдущий стол", CTRL | ALT, k::KEY_Left;
    NextSpace, "next-space", "Следующий стол", CTRL | ALT, k::KEY_Right;
    MovePrevious, "move-previous", "Окно на предыдущий стол", CTRL | ALT | SHIFT, k::KEY_Left;
    MoveNext, "move-next", "Окно на следующий стол", CTRL | ALT | SHIFT, k::KEY_Right;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bindings(Vec<(Action, Option<Chord>)>);
impl Bindings {
    pub fn chord(&self, action: Action) -> Option<Chord> {
        self.0
            .iter()
            .find(|(a, _)| *a == action)
            .map_or(Some(action.default_chord()), |(_, c)| *c)
    }
    fn matches(&self, action: Action, chord: Chord) -> bool {
        self.chord(action) == Some(chord)
            || action == Action::Terminal
                && !self.0.iter().any(|(a, _)| *a == action)
                && chord
                    == (Chord {
                        mods: SUPER,
                        key: k::KEY_t,
                    })
    }
    pub fn action(&self, chord: Chord) -> Option<Action> {
        Action::ALL
            .iter()
            .copied()
            .find(|a| self.matches(*a, chord))
    }
    pub fn set(&mut self, action: Action, chord: Option<Chord>) -> Result<(), String> {
        if let Some(chord) = chord {
            if !chord.valid() {
                return Err("Выберите сочетание с Ctrl, Alt или Super; системные сочетания зарезервированы.".into());
            }
            if let Some(other) = Action::ALL
                .iter()
                .copied()
                .find(|a| *a != action && self.matches(*a, chord))
            {
                return Err(format!("Это сочетание занято: «{}».", other.label()));
            }
        }
        self.0.retain(|(a, _)| *a != action);
        self.0.push((action, chord));
        Ok(())
    }
    pub fn reset(&mut self) {
        self.0.clear();
    }
    pub fn lines(&self) -> impl Iterator<Item = String> + '_ {
        self.0.iter().map(|(a, chord)| {
            format!(
                "keybinding={};{}\n",
                a.id(),
                chord.map_or("off".into(), |c| format!("{};{}", c.mods, c.key))
            )
        })
    }
    pub fn parse_line(&mut self, text: &str) {
        let Some((action, value)) = text.split_once(';') else {
            return;
        };
        let Some(action) = Action::parse(action) else {
            return;
        };
        let chord = if value == "off" {
            None
        } else {
            let Some((mods, key)) = value.split_once(';') else {
                return;
            };
            let (Ok(mods), Ok(key)) = (mods.parse(), key.parse()) else {
                return;
            };
            let chord = Chord { mods, key };
            if !chord.valid() {
                return;
            }
            Some(chord)
        };
        // Resolve conflicts after reading all records: swapping two defaults must work.
        self.0.retain(|(a, _)| *a != action);
        self.0.push((action, chord));
    }
    pub fn normalize(&mut self) {
        // Invalid/conflicting hand-edited configurations fall back as a whole.
        for action in Action::ALL.iter().copied() {
            if let Some(chord) = self.chord(action) {
                if Action::ALL
                    .iter()
                    .copied()
                    .any(|a| a != action && self.matches(a, chord))
                {
                    self.reset();
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conflicts_reserved_keys_disabling_and_latin_matching() {
        let mut b = Bindings::default();
        assert!(b
            .set(Action::Files, Some(Action::Search.default_chord()))
            .is_err());
        assert!(b
            .set(
                Action::Files,
                Some(Chord {
                    mods: SUPER,
                    key: k::KEY_t
                })
            )
            .is_err());
        for c in [
            Chord {
                mods: SUPER,
                key: k::KEY_l,
            },
            Chord {
                mods: CTRL | ALT,
                key: k::KEY_F3,
            },
            Chord {
                mods: 0,
                key: k::KEY_a,
            },
        ] {
            assert!(b.set(Action::Files, Some(c)).is_err());
        }
        b.set(Action::Search, None).unwrap();
        b.set(Action::Files, Some(Action::Search.default_chord()))
            .unwrap();
        assert_eq!(
            b.action(Action::Search.default_chord()),
            Some(Action::Files)
        );
        assert_eq!(b.chord(Action::Search), None);
        let chord = Chord::new(
            &ModifiersState {
                logo: true,
                shift: true,
                ..Default::default()
            },
            k::KEY_E,
        );
        assert_eq!(
            chord,
            Chord {
                mods: SUPER | SHIFT,
                key: k::KEY_e
            }
        );
        assert_eq!(chord.label(), "Shift + Super + E");
    }
    #[test]
    fn persistence_swaps_and_invalid_records() {
        let mut b = Bindings::default();
        b.parse_line("search;8;101");
        b.parse_line("files;8;32");
        b.normalize();
        assert_eq!(
            b.action(Chord {
                mods: SUPER,
                key: k::KEY_e
            }),
            Some(Action::Search)
        );
        let mut back = Bindings::default();
        for line in b.lines() {
            back.parse_line(line.trim().strip_prefix("keybinding=").unwrap());
        }
        back.normalize();
        assert_eq!(back, b);
        for line in [
            "unknown;off",
            "files;999;32",
            "files;8;108",
            "files;8;999999",
            "files;bad",
        ] {
            back.parse_line(line);
        }
        assert_eq!(back, b);
        back.parse_line("search;8;32");
        back.normalize();
        assert_eq!(back, Bindings::default());
    }
}

impl crate::FeatherCompositor {
    pub(crate) fn capturing_shortcut(&self) -> bool {
        self.system_settings.shortcut_capture.is_some()
            && self.system_settings.page == crate::system_settings::Page::Keyboard
            && !self.lock.active
            && !self.auth.is_open()
            && !self.shell.spotlight_open
            && self.shell.launchpad.is_none()
            && !self.shell.control_center_open
            && self
                .windows
                .iter()
                .rev()
                .find(|w| w.is_visible())
                .is_some_and(|w| w.app_id == "builtin.settings")
    }
    pub(crate) fn capture_shortcut(&mut self, sym: u32, latin: u32, mods: &ModifiersState) {
        let Some(action) = self.system_settings.shortcut_capture else {
            return;
        };
        if (k::KEY_Shift_L..=k::KEY_Hyper_R).contains(&sym)
            || matches!(sym, k::KEY_ISO_Level3_Shift | k::KEY_Caps_Lock)
        {
            return;
        }
        if sym == k::KEY_Escape {
            self.system_settings.shortcut_capture = None;
            self.system_settings.error = None;
        } else {
            let chord = if matches!(sym, k::KEY_BackSpace | k::KEY_Delete)
                && !mods.ctrl
                && !mods.alt
                && !mods.logo
            {
                None
            } else {
                Some(Chord::new(mods, latin))
            };
            match self.keybindings.set(action, chord) {
                Ok(()) => {
                    self.system_settings.shortcut_capture = None;
                    self.system_settings.error = None;
                    self.system_settings.keybindings = self.keybindings.clone();
                    self.save_settings();
                }
                Err(e) => self.system_settings.error = Some(e),
            }
        }
        self.mark_settings_dirty();
        self.needs_redraw = true;
    }
    pub(crate) fn shortcut_action(&mut self, action: Action) -> Option<crate::input::KeyAction> {
        use crate::{apps::AppAction, input::KeyAction as A, shell::ShellClickAction as S};
        Some(match action {
            Action::Search => {
                self.shell.spotlight_open = !self.shell.spotlight_open;
                self.shell.control_center_open = false;
                self.shell.open_menu = None;
                self.shell.overlay_dirty = true;
                self.shell.topbar_dirty = true;
                return None;
            }
            Action::Applications => {
                A::Shell(S::LaunchApp(AppAction::OpenLaunchpad, "Приложения".into()))
            }
            Action::Terminal => A::Shell(S::LaunchApp(
                AppAction::Exec(self.system_preferences.terminal_command()),
                "Терминал".into(),
            )),
            Action::Files => A::Shell(S::LaunchApp(
                AppAction::Exec(crate::apps::files_command()),
                "Файлы".into(),
            )),
            Action::ControlCenter => {
                self.shell.control_center_open = !self.shell.control_center_open;
                self.shell.overlay_dirty = true;
                return None;
            }
            Action::Close => A::CloseTop,
            Action::Quit => A::Shell(S::QuitFocusedApp),
            Action::Minimize => A::MinimizeTop,
            Action::Maximize => A::ToggleMaximizeTop,
            Action::Hide => A::Shell(S::HideApp),
            Action::HideOthers => A::Shell(S::HideOthers),
            Action::Mission => A::Mission {
                focus_hover: false,
                close_only: false,
            },
            Action::PreviousSpace | Action::MovePrevious => A::Space {
                delta: -1,
                carry: action == Action::MovePrevious,
            },
            Action::NextSpace | Action::MoveNext => A::Space {
                delta: 1,
                carry: action == Action::MoveNext,
            },
        })
    }
}
