//! Built-in interactive Keyboard Shortcuts & Gestures Cheat Sheet («Горячие клавиши Пёрышка»).
//!
//! Zero background memory or idle CPU cost: all catalog metadata lives in `&'static` tables,
//! and pixels are rasterized only when the window is visible and marked `dirty`.

use tiny_skia::PixmapMut;

use crate::menu_hud::flip_keyboard_layout;
use crate::shortcuts::{Action as ShortcutAction, Bindings, ALT, CTRL, SHIFT, SUPER};
use crate::text::FontSystem;
use crate::theme::{
    appearance, fill_circle, fill_rounded_rect, high_contrast, palette, rgba, stroke_circle,
    stroke_line, stroke_rounded_rect, Appearance,
};

const HEADER_H: f32 = 92.0;
const PAD_X: f32 = 18.0;
const SECTION_TITLE_H: f32 = 28.0;
const SECTION_GAP: f32 = 14.0;
const ROW_H: f32 = 44.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Category {
    #[default]
    All,
    Shell,
    Spaces,
    Windows,
    System,
    Apps,
}

impl Category {
    pub const ALL: [Self; 6] = [
        Self::All,
        Self::Shell,
        Self::Spaces,
        Self::Windows,
        Self::System,
        Self::Apps,
    ];

    pub const fn pill_label(self) -> &'static str {
        match self {
            Self::All => "Все",
            Self::Shell => "Оболочка и шторки",
            Self::Spaces => "Столы и тайлинг",
            Self::Windows => "Окна и PiP",
            Self::System => "Захват и система",
            Self::Apps => "Программы",
        }
    }

    pub const fn section_title(self) -> &'static str {
        match self {
            Self::All => "Все горячие клавиши",
            Self::Shell => "Оболочка, поиск и выдвижные шторки",
            Self::Spaces => "Рабочие столы, Авто-плитка и Бесконечная лента",
            Self::Windows => "Окна, прижатие к краям и Призрачный PiP",
            Self::System => "Снимки, запись экрана и система",
            Self::Apps => "Быстрый запуск и встроенные программы",
        }
    }

    pub fn step(self, reverse: bool) -> Self {
        let idx = Self::ALL.iter().position(|&c| c == self).unwrap_or(0);
        let next = if reverse {
            (idx + Self::ALL.len() - 1) % Self::ALL.len()
        } else {
            (idx + 1) % Self::ALL.len()
        };
        Self::ALL[next]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HitAction {
    SelectCategory(Category),
    ClearSearch,
    FocusSearch {
        rel_x: f32,
        width: f32,
        shift: bool,
    },
    OpenKeyboardSettings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    Ignored,
    Repaint,
    CloseWindow,
}

#[derive(Clone, Copy)]
enum KeySpec {
    Bound(ShortcutAction),
    SpacePair { carry: bool },
    InputSwitch,
    Fixed(&'static [&'static str]),
}

struct Entry {
    category: Category,
    title: &'static str,
    desc: &'static str,
    keywords: &'static str,
    keys: KeySpec,
}

const ENTRIES: &[Entry] = &[
    // ── Оболочка, поиск и выдвижные шторки ──
    Entry {
        category: Category::Shell,
        title: "Кольцо программ",
        desc: "Круговое меню у курсора; двойное нажатие Super — возврат к прошлому окну",
        keywords: "ring radial super tap кольцо радиальное переключение",
        keys: KeySpec::Fixed(&["Super"]),
    },
    Entry {
        category: Category::Shell,
        title: "Spotlight и быстрый калькулятор",
        desc: "Мгновенный поиск программ, файлов, настроек и вычисления прямо в строке",
        keywords: "spotlight search calc поиск файлы калькулятор пробел space",
        keys: KeySpec::Bound(ShortcutAction::Search),
    },
    Entry {
        category: Category::Shell,
        title: "Поиск по командам меню (Menu HUD)",
        desc: "Мгновенный поиск любого пункта верхнего меню активной программы и оболочки",
        keywords: "menu hud help команды меню справка slash",
        keys: KeySpec::Bound(ShortcutAction::MenuHud),
    },
    Entry {
        category: Category::Shell,
        title: "Памятка горячих клавиш",
        desc: "Открыть или скрыть это окно со всеми сочетаниями (также работает Super + F1)",
        keywords: "shortcuts cheat sheet help f1 памятка клавиши справка хоткеи",
        keys: KeySpec::Bound(ShortcutAction::ShortcutsGuide),
    },
    Entry {
        category: Category::Shell,
        title: "Launchpad (сетка приложений)",
        desc: "Полноэкранная сетка всех установленных программ с поиском и страницами",
        keywords: "launchpad apps grid приложения программы сетка",
        keys: KeySpec::Bound(ShortcutAction::Applications),
    },
    Entry {
        category: Category::Shell,
        title: "Верхняя шторка (Пункт управления)",
        desc: "Wi-Fi, Bluetooth, звук, микшер громкости по программам, яркость и экран",
        keywords: "control center drawer wifi bluetooth sound mixer шторка пункт управления звук микшер",
        keys: KeySpec::Bound(ShortcutAction::ControlCenter),
    },
    Entry {
        category: Category::Shell,
        title: "Шторка-Подложка (Scratchpad)",
        desc: "Выдвижной рабочий слой поверх любого стола для мессенджера, заметок или терминала",
        keywords: "scratchpad drawer overlay шторка подложка выдвижная",
        keys: KeySpec::Bound(ShortcutAction::ScratchToggle),
    },
    Entry {
        category: Category::Shell,
        title: "Убрать окно на Подложку и обратно",
        desc: "Перенести активное окно на шторку-Подложку или вернуть на текущий рабочий стол",
        keywords: "scratchpad move send шторка подложка перенести отправить",
        keys: KeySpec::Bound(ShortcutAction::ScratchMove),
    },
    Entry {
        category: Category::Shell,
        title: "Полка перетаскивания (Drop Shelf)",
        desc: "Временный буфер файлов и текста; также открывается встряхиванием курсора с файлом",
        keywords: "drop shelf yoink drag shake полка перетаскивание файлы буфер встряхивание",
        keys: KeySpec::Bound(ShortcutAction::DropShelf),
    },
    Entry {
        category: Category::Shell,
        title: "История буфера обмена",
        desc: "Список недавно скопированных текстов и быстрая вставка выбранного фрагмента",
        keywords: "clipboard history copy paste буфер обмена история вставка",
        keys: KeySpec::Bound(ShortcutAction::ClipboardHistory),
    },
    Entry {
        category: Category::Shell,
        title: "Панель эмодзи и символов",
        desc: "Поиск смайлов и спецсимволов по названию и вставка в активное поле ввода",
        keywords: "emoji symbols picker эмодзи смайлы символы точка",
        keys: KeySpec::Bound(ShortcutAction::Emoji),
    },

    // ── Рабочие столы, Авто-плитка и Бесконечная лента ──
    Entry {
        category: Category::Spaces,
        title: "Прямой переход на стол 1…9",
        desc: "Мгновенное переключение на нужный рабочий стол с карточкой-индикатором по центру",
        keywords: "spaces desktops switch numbers 1 9 рабочие столы переход цифры",
        keys: KeySpec::Fixed(&["Super", "1…9"]),
    },
    Entry {
        category: Category::Spaces,
        title: "Перенести окно на стол 1…9",
        desc: "Отправить активное окно на выбранный рабочий стол и сразу перейти за ним",
        keywords: "move window space desktop 1 9 перенести окно стол",
        keys: KeySpec::Fixed(&["Super", "Shift", "1…9"]),
    },
    Entry {
        category: Category::Spaces,
        title: "Соседний рабочий стол",
        desc: "Шаг влево или вправо между столами (или горизонтальный свайп 3 пальцами по трекпаду)",
        keywords: "next previous space swipe 3 fingers соседний стол свайп трекпад",
        keys: KeySpec::SpacePair { carry: false },
    },
    Entry {
        category: Category::Spaces,
        title: "Окно на соседний рабочий стол",
        desc: "Перенести активное окно на стол слева или справа вместе с фокусом",
        keywords: "move window next previous space перенести соседний стол",
        keys: KeySpec::SpacePair { carry: true },
    },
    Entry {
        category: Category::Spaces,
        title: "Обзор окон и столов (Mission Control)",
        desc: "Все открытые окна и полоса рабочих столов сверху (или свайп 3 пальцами вверх)",
        keywords: "mission control overview expose мишн контрол обзор окон столов",
        keys: KeySpec::Bound(ShortcutAction::Mission),
    },
    Entry {
        category: Category::Spaces,
        title: "Режим раскладки рабочего стола",
        desc: "Переключить текущий стол: Свободные окна → Авто-плитка → Бесконечная лента",
        keywords: "space mode tiling bsp ribbon tape niri floating режим стола авто-плитка тайлинг лента",
        keys: KeySpec::Bound(ShortcutAction::SpaceMode),
    },
    Entry {
        category: Category::Spaces,
        title: "Фокус соседней плитки или колонки",
        desc: "Переход между окнами по стрелкам в режимах Авто-плитки, Ленты и на Подложке",
        keywords: "focus tile column arrows фокус плитка колонка стрелки тайлинг",
        keys: KeySpec::Fixed(&["Super", "← ↑ ↓ →"]),
    },
    Entry {
        category: Category::Spaces,
        title: "Обмен окон / поворот оси сплита",
        desc: "Переставить колонки в Ленте или поменять местами окна и развернуть ось в Авто-плитке",
        keywords: "swap rotate split tile column поменять местами поворот сплит плитка лента",
        keys: KeySpec::Fixed(&["Super", "Shift", "← ↑ ↓ →"]),
    },
    Entry {
        category: Category::Spaces,
        title: "Изменить размер плитки или колонки",
        desc: "Плавно сдвинуть границу сплита (также перетаскиванием зазора между окнами мышью)",
        keywords: "resize split divider tile column размер граница зазор плитка колонка",
        keys: KeySpec::Fixed(&["Ctrl", "Super", "← ↑ ↓ →"]),
    },
    Entry {
        category: Category::Spaces,
        title: "Прокрутка Бесконечной ленты",
        desc: "Плавный горизонтальный скроллинг ленты окон колесом мыши или трекпадом",
        keywords: "scroll ribbon tape wheel touchpad скроллинг прокрутка бесконечная лента колесо",
        keys: KeySpec::Fixed(&["Super", "Колесо / Трекпад"]),
    },
    Entry {
        category: Category::Spaces,
        title: "Ширина колонки в Бесконечной ленте",
        desc: "Переключить пресет ширины активного окна по кругу: 50 % → 67 % → 100 % → 33 %",
        keywords: "column width preset ribbon tape ширина колонки пресет лента",
        keys: KeySpec::Fixed(&["Super", "R"]),
    },
    Entry {
        category: Category::Spaces,
        title: "Края ленты и выравнивание 50 / 50",
        desc: "Super + Home / End — к первому или последнему окну ленты; Super + 0 — сброс пропорций",
        keywords: "home end equalize 50 reset начало конец выровнять пропорции сброс",
        keys: KeySpec::Fixed(&["Super", "0 / Home / End"]),
    },

    // ── Окна, прижатие к краям и Призрачный PiP ──
    Entry {
        category: Category::Windows,
        title: "Переключатель программ",
        desc: "Быстрый переход между открытыми приложениями (также работает Super + Tab)",
        keywords: "alt tab app switcher переключатель программ приложений",
        keys: KeySpec::Fixed(&["Alt", "Tab"]),
    },
    Entry {
        category: Category::Windows,
        title: "Окна текущей программы",
        desc: "Переключение только между окнами активного приложения (также Super + `)",
        keywords: "cycle app windows grave tilde окна текущей программы тильда ё",
        keys: KeySpec::Fixed(&["Alt", "`"]),
    },
    Entry {
        category: Category::Windows,
        title: "Прижать окно к половине или четверти",
        desc: "На свободном столе: прижать к краю экрана; повтор со смежной стрелкой — в угол (1/4)",
        keywords: "snap half quarter tile edge corner прижать половина четверть угол край",
        keys: KeySpec::Fixed(&["Ctrl", "Super", "← ↑ ↓ →"]),
    },
    Entry {
        category: Category::Windows,
        title: "Перетащить / изменить размер за любую точку",
        desc: "Зажмите Super: левая кнопка мыши двигает окно, правая кнопка меняет его размер",
        keywords: "drag move resize mouse super lmb rmb перетащить размер мышь любая точка",
        keys: KeySpec::Fixed(&["Super", "ЛКМ / ПКМ"]),
    },
    Entry {
        category: Category::Windows,
        title: "Картинка в картинке (PiP)",
        desc: "Закрепить компактное окно поверх всех рабочих столов с прилипанием к углам экрана",
        keywords: "pip picture in picture sticky pin картинка в картинке поверх всех столов",
        keys: KeySpec::Bound(ShortcutAction::Pip),
    },
    Entry {
        category: Category::Windows,
        title: "Призрачный PiP («Сквозь курсор»)",
        desc: "Полупрозрачное окно пропускает все клики насквозь; зажмите Super, чтобы перетащить",
        keywords: "ghost pip click through translucent призрачный сквозь курсор прозрачное",
        keys: KeySpec::Bound(ShortcutAction::PipGhost),
    },
    Entry {
        category: Category::Windows,
        title: "Развернуть / увеличить окно",
        desc: "Развернуть окно на рабочую область (в Авто-плитке и Ленте — временный зум на весь стол)",
        keywords: "maximize zoom fullscreen развернуть увеличить весь экран зум",
        keys: KeySpec::Bound(ShortcutAction::Maximize),
    },
    Entry {
        category: Category::Windows,
        title: "Свернуть окно в Dock",
        desc: "Плавно убрать активное окно в правую секцию панели Dock",
        keywords: "minimize dock свернуть док",
        keys: KeySpec::Bound(ShortcutAction::Minimize),
    },
    Entry {
        category: Category::Windows,
        title: "Скрыть программу (Super + Alt + H — остальные)",
        desc: "Спрятать все окна активного приложения или оставить на экране только его",
        keywords: "hide app others скрыть программу остальные",
        keys: KeySpec::Bound(ShortcutAction::Hide),
    },
    Entry {
        category: Category::Windows,
        title: "Закрыть активное окно",
        desc: "Закрыть текущее окно, не завершая остальные окна программы",
        keywords: "close window закрыть окно",
        keys: KeySpec::Bound(ShortcutAction::Close),
    },
    Entry {
        category: Category::Windows,
        title: "Завершить программу",
        desc: "Закрыть все окна активного приложения",
        keywords: "quit exit app завершить выйти программа",
        keys: KeySpec::Bound(ShortcutAction::Quit),
    },

    // ── Снимки, запись экрана и система ──
    Entry {
        category: Category::System,
        title: "Снимок всего экрана",
        desc: "Сохранить PNG в папку снимков и сразу скопировать изображение в буфер обмена",
        keywords: "screenshot full screen print снимок скриншот весь экран",
        keys: KeySpec::Fixed(&["Print"]),
    },
    Entry {
        category: Category::System,
        title: "Снимок выбранной области",
        desc: "Выделить прямоугольник на экране мышью (Esc — отменить выделение)",
        keywords: "screenshot area region selection shift print снимок области выделение",
        keys: KeySpec::Fixed(&["Shift", "Print"]),
    },
    Entry {
        category: Category::System,
        title: "Снимок активного окна",
        desc: "Чистый кадр сфокусированного окна без лишнего фона",
        keywords: "screenshot window alt print снимок окна",
        keys: KeySpec::Fixed(&["Alt", "Print"]),
    },
    Entry {
        category: Category::System,
        title: "Панель записи видео с экрана",
        desc: "Запись экрана, окна или области в MP4 со звуком (также Ctrl + Shift + Print)",
        keywords: "screen recorder video mp4 audio screencast запись экрана видео звук",
        keys: KeySpec::Bound(ShortcutAction::ScreenRecorder),
    },
    Entry {
        category: Category::System,
        title: "Смена раскладки клавиатуры",
        desc: "Переключение языка ввода (RU / EN); сочетание выбирается в «Настройках → Клавиатура»",
        keywords: "layout language input ru en раскладка язык переключение клавиатура",
        keys: KeySpec::InputSwitch,
    },
    Entry {
        category: Category::System,
        title: "Заблокировать экран",
        desc: "Мгновенно закрыть сеанс экраном блокировки с часами и вводом пароля",
        keywords: "lock screen password заблокировать экран блокировка пароль",
        keys: KeySpec::Fixed(&["Super", "L"]),
    },
    Entry {
        category: Category::System,
        title: "Мониторинг системы (Диспетчер задач)",
        desc: "Нагрузка на ЦП и память, список процессов и принудительное завершение зависших задач",
        keywords: "task manager activity monitor processes cpu ram диспетчер задач мониторинг процессы",
        keys: KeySpec::Fixed(&["Ctrl", "Alt", "Del"]),
    },
    Entry {
        category: Category::System,
        title: "Масштаб интерфейса (HiDPI)",
        desc: "Быстрое переключение чёткого масштаба экрана на лету",
        keywords: "scale hidpi dpi zoom масштаб экрана интерфейс",
        keys: KeySpec::Fixed(&["Super", "+ / −"]),
    },
    Entry {
        category: Category::System,
        title: "Перезагрузить оболочку / Выйти из сеанса",
        desc: "Super + Shift + R обновляет панель без закрытия окон; Ctrl + Alt + Backspace — выход",
        keywords: "reload shell logout session перезагрузить оболочку выйти сеанс",
        keys: KeySpec::Fixed(&["Super", "Shift", "R"]),
    },

    // ── Быстрый запуск и встроенные программы ──
    Entry {
        category: Category::Apps,
        title: "Открыть Терминал",
        desc: "Запуск терминала (также работает Super + T; внутри терминала Ctrl + Shift + T — вкладка)",
        keywords: "terminal console shell терминал консоль вкладка",
        keys: KeySpec::Bound(ShortcutAction::Terminal),
    },
    Entry {
        category: Category::Apps,
        title: "Открыть Файлы",
        desc: "Запуск файлового менеджера Пёрышка",
        keywords: "files finder folder файлы проводник папки",
        keys: KeySpec::Bound(ShortcutAction::Files),
    },
    Entry {
        category: Category::Apps,
        title: "Файлы: Быстрый просмотр и навигация",
        desc: "Пробел — предпросмотр файла, Ctrl + L — путь, Ctrl + H — скрытые файлы, F2 — имя",
        keywords: "quick look preview files space rename быстрый просмотр файлы пробел скрытые",
        keys: KeySpec::Fixed(&["Пробел"]),
    },
    Entry {
        category: Category::Apps,
        title: "Терминал: буфер обмена и поиск",
        desc: "Ctrl + Shift + C / V — копировать и вставить, Ctrl + Shift + F — поиск по выводу",
        keywords: "terminal copy paste search терминал копировать вставить поиск",
        keys: KeySpec::Fixed(&["Ctrl", "Shift", "C / V"]),
    },
    Entry {
        category: Category::Apps,
        title: "Текстовый редактор и Заметки",
        desc: "Ctrl + N — новый документ, Ctrl + S — сохранить, Ctrl + F — поиск, Ctrl + Z — отменить",
        keywords: "textedit notes save new find текстовый редактор заметки сохранить поиск",
        keys: KeySpec::Fixed(&["Ctrl", "S"]),
    },
    Entry {
        category: Category::Apps,
        title: "Рабочий стол ИИ-помощника (Ларвис)",
        desc: "Переключиться на изолированный рабочий стол автономного агента и вернуться обратно",
        keywords: "larvis ai agent desktop ларвис агент помощник",
        keys: KeySpec::Bound(ShortcutAction::LarvisDesktop),
    },
];

pub struct ShortcutsGuide {
    pub category: Category,
    pub input: feather_ui::input::TextInput,
    pub scroll: f32,
}

impl Default for ShortcutsGuide {
    fn default() -> Self {
        Self {
            category: Category::All,
            input: feather_ui::input::TextInput::new("").with_limit(64),
            scroll: 0.0,
        }
    }
}

enum ResolvedKeys {
    Caps(Vec<String>),
    Disabled,
}

fn resolve_chord_tokens(chord: crate::shortcuts::Chord) -> Vec<String> {
    use smithay::input::keyboard::keysyms as k;
    let mut parts = Vec::with_capacity(4);
    for (bit, name) in [
        (CTRL, "Ctrl"),
        (ALT, "Alt"),
        (SHIFT, "Shift"),
        (SUPER, "Super"),
    ] {
        if chord.mods & bit != 0 {
            parts.push(name.to_string());
        }
    }
    let key_name = match chord.key {
        k::KEY_space => feather_ui::i18n::tr("Пробел").into_owned(),
        k::KEY_Return => "Enter".into(),
        k::KEY_Left => "←".into(),
        k::KEY_Right => "→".into(),
        k::KEY_Up => "↑".into(),
        k::KEY_Down => "↓".into(),
        v if (k::KEY_F1..=k::KEY_F12).contains(&v) => format!("F{}", v - k::KEY_F1 + 1),
        v => char::from_u32(v)
            .map(|c| c.to_ascii_uppercase().to_string())
            .unwrap_or_default(),
    };
    if !key_name.is_empty() {
        parts.push(key_name);
    }
    parts
}

fn resolve_keys(spec: KeySpec, bindings: &Bindings, input_shortcut: &str) -> ResolvedKeys {
    match spec {
        KeySpec::Bound(action) => match bindings.chord(action) {
            Some(c) => ResolvedKeys::Caps(resolve_chord_tokens(c)),
            None => ResolvedKeys::Disabled,
        },
        KeySpec::SpacePair { carry } => {
            let (prev_act, next_act) = if carry {
                (ShortcutAction::MovePrevious, ShortcutAction::MoveNext)
            } else {
                (ShortcutAction::PreviousSpace, ShortcutAction::NextSpace)
            };
            let prev_default = bindings.chord(prev_act) == Some(prev_act.default_chord());
            let next_default = bindings.chord(next_act) == Some(next_act.default_chord());
            if prev_default && next_default {
                let mut v = vec!["Ctrl".into(), "Alt".into()];
                if carry {
                    v.push("Shift".into());
                }
                v.push("← / →".into());
                ResolvedKeys::Caps(v)
            } else if let Some(c) = bindings.chord(next_act).or_else(|| bindings.chord(prev_act)) {
                ResolvedKeys::Caps(resolve_chord_tokens(c))
            } else {
                ResolvedKeys::Disabled
            }
        }
        KeySpec::InputSwitch => {
            let trimmed = input_shortcut.trim();
            if trimmed.is_empty() {
                ResolvedKeys::Caps(vec!["Alt".into(), "Shift".into()])
            } else {
                let caps = trimmed
                    .split('+')
                    .map(|part| feather_ui::i18n::tr(part.trim()).into_owned())
                    .collect();
                ResolvedKeys::Caps(caps)
            }
        }
        KeySpec::Fixed(tokens) => ResolvedKeys::Caps(
            tokens
                .iter()
                .map(|t| feather_ui::i18n::tr(t).into_owned())
                .collect(),
        ),
    }
}

fn matches_query_terms(
    entry: &Entry,
    resolved: &ResolvedKeys,
    terms: &[&str],
) -> bool {
    if terms.is_empty() {
        return true;
    }
    let title_ru = entry.title.to_lowercase();
    let desc_ru = entry.desc.to_lowercase();
    let title_loc = feather_ui::i18n::tr_static(entry.title).to_lowercase();
    let desc_loc = feather_ui::i18n::tr_static(entry.desc).to_lowercase();
    let kw = entry.keywords;
    let key_str = match resolved {
        ResolvedKeys::Caps(caps) => caps.join("+").to_lowercase(),
        ResolvedKeys::Disabled => String::new(),
    };
    let key_spaced = match resolved {
        ResolvedKeys::Caps(caps) => caps.join(" ").to_lowercase(),
        ResolvedKeys::Disabled => String::new(),
    };
    terms.iter().all(|term| {
        title_ru.contains(term)
            || desc_ru.contains(term)
            || title_loc.contains(term)
            || desc_loc.contains(term)
            || kw.contains(term)
            || key_str.contains(term)
            || key_spaced.contains(term)
    })
}

struct HeaderGeometry {
    settings_btn: (f32, f32, f32, f32),
    search_box: (f32, f32, f32, f32),
    search_text_x: f32,
    search_text_w: f32,
    clear_btn: Option<(f32, f32, f32, f32)>,
    pills: Vec<(Category, (f32, f32, f32, f32))>,
}

impl ShortcutsGuide {
    fn header_geometry(
        &self,
        fonts: &mut FontSystem,
        cx: f32,
        cy: f32,
        cw: f32,
    ) -> HeaderGeometry {
        let btn_label = "Настроить…";
        let btn_w = (fonts.measure_text(btn_label, 12.0, true) + 24.0).clamp(96.0, 148.0);
        let btn_h = 28.0;
        let btn_x = cx + cw - PAD_X - btn_w;
        let btn_y = cy + 13.0;

        let search_w = (cw * 0.31).clamp(168.0, 256.0);
        let search_h = 28.0;
        let search_x = (btn_x - 8.0 - search_w).max(cx + PAD_X + 180.0);
        let search_y = cy + 13.0;

        let has_query = !self.input.is_empty();
        let search_text_x = search_x + 28.0;
        let right_pad = if has_query { 26.0 } else { 10.0 };
        let search_text_w = (search_w - 28.0 - right_pad).max(36.0);
        let clear_btn = has_query.then_some((
            search_x + search_w - 24.0,
            search_y + 4.0,
            20.0,
            20.0,
        ));

        let pill_y = cy + 54.0;
        let pill_h = 25.0;
        let mut px = cx + PAD_X;
        let mut pills = Vec::with_capacity(Category::ALL.len());
        for cat in Category::ALL {
            let w = (fonts.measure_text(cat.pill_label(), 12.0, self.category == cat) + 22.0)
                .max(44.0);
            if px + w <= cx + cw - PAD_X + 2.0 {
                pills.push((cat, (px, pill_y, w, pill_h)));
                px += w + 6.0;
            }
        }

        HeaderGeometry {
            settings_btn: (btn_x, btn_y, btn_w, btn_h),
            search_box: (search_x, search_y, search_w, search_h),
            search_text_x,
            search_text_w,
            clear_btn,
            pills,
        }
    }

    fn filtered_sections(
        &self,
        bindings: &Bindings,
        input_shortcut: &str,
    ) -> Vec<(Category, Vec<(&'static Entry, ResolvedKeys)>)> {
        let q = self.input.text().trim().to_lowercase();
        let terms: Vec<&str> = q.split_whitespace().collect();

        let collect = |terms: &[&str]| -> Vec<(Category, Vec<(&'static Entry, ResolvedKeys)>)> {
            let categories: &[Category] = match &self.category {
                Category::All => &[
                    Category::Shell,
                    Category::Spaces,
                    Category::Windows,
                    Category::System,
                    Category::Apps,
                ],
                single => std::slice::from_ref(single),
            };
            let mut out = Vec::new();
            for &cat in categories {
                let mut rows = Vec::new();
                for entry in ENTRIES.iter().filter(|e| e.category == cat) {
                    let resolved = resolve_keys(entry.keys, bindings, input_shortcut);
                    if matches_query_terms(entry, &resolved, terms) {
                        rows.push((entry, resolved));
                    }
                }
                if !rows.is_empty() {
                    out.push((cat, rows));
                }
            }
            out
        };

        let mut sections = collect(&terms);
        if sections.is_empty() && !q.is_empty() {
            if let Some(flipped) = flip_keyboard_layout(&q) {
                let flipped_terms: Vec<&str> = flipped.split_whitespace().collect();
                sections = collect(&flipped_terms);
            }
        }
        sections
    }

    pub fn content_height(&self, bindings: &Bindings, input_shortcut: &str) -> f32 {
        let sections = self.filtered_sections(bindings, input_shortcut);
        if sections.is_empty() {
            return 160.0;
        }
        let mut h = 14.0;
        for (i, (_, rows)) in sections.iter().enumerate() {
            if i > 0 {
                h += SECTION_GAP;
            }
            h += SECTION_TITLE_H + rows.len() as f32 * ROW_H;
        }
        h + 20.0
    }

    pub fn max_scroll(&self, ch: f32, bindings: &Bindings, input_shortcut: &str) -> f32 {
        let view_h = (ch - HEADER_H).max(60.0);
        (self.content_height(bindings, input_shortcut) - view_h).max(0.0)
    }

    pub fn scroll_by(
        &mut self,
        delta: f64,
        ch: f32,
        bindings: &Bindings,
        input_shortcut: &str,
    ) -> bool {
        let max = self.max_scroll(ch, bindings, input_shortcut);
        let old = self.scroll;
        self.scroll = (old + delta as f32 * 3.2).clamp(0.0, max);
        (self.scroll - old).abs() > 0.01
    }

    pub fn hit(
        &self,
        fonts: &mut FontSystem,
        lx: f32,
        ly: f32,
        cw: f32,
        shift: bool,
    ) -> Option<HitAction> {
        if ly < 0.0 || ly > HEADER_H {
            return None;
        }
        let g = self.header_geometry(fonts, 0.0, 0.0, cw);
        let inside = |x: f32, y: f32, (rx, ry, rw, rh): (f32, f32, f32, f32)| {
            x >= rx && x <= rx + rw && y >= ry && y <= ry + rh
        };
        if inside(lx, ly, g.settings_btn) {
            return Some(HitAction::OpenKeyboardSettings);
        }
        if let Some(clear) = g.clear_btn {
            if inside(lx, ly, clear) {
                return Some(HitAction::ClearSearch);
            }
        }
        if inside(lx, ly, g.search_box) {
            return Some(HitAction::FocusSearch {
                rel_x: (lx - g.search_text_x).max(0.0),
                width: g.search_text_w,
                shift,
            });
        }
        for (cat, rect) in g.pills {
            if inside(lx, ly, rect) {
                return Some(HitAction::SelectCategory(cat));
            }
        }
        None
    }

    pub fn on_key(
        &mut self,
        sym: smithay::input::keyboard::Keysym,
        latin_sym: smithay::input::keyboard::Keysym,
        ctrl: bool,
        shift: bool,
        ch: f32,
        bindings: &Bindings,
        input_shortcut: &str,
    ) -> KeyOutcome {
        use smithay::input::keyboard::keysyms as k;
        let max = self.max_scroll(ch, bindings, input_shortcut);
        match sym.raw() {
            k::KEY_Escape => {
                if !self.input.is_empty() {
                    self.input.set("");
                    self.scroll = 0.0;
                    KeyOutcome::Repaint
                } else {
                    KeyOutcome::CloseWindow
                }
            }
            k::KEY_Tab => {
                self.category = self.category.step(shift);
                self.scroll = 0.0;
                KeyOutcome::Repaint
            }
            k::KEY_ISO_Left_Tab => {
                self.category = self.category.step(true);
                self.scroll = 0.0;
                KeyOutcome::Repaint
            }
            k::KEY_Up => {
                let next = (self.scroll - 48.0).clamp(0.0, max);
                self.scroll = next;
                KeyOutcome::Repaint
            }
            k::KEY_Down => {
                let next = (self.scroll + 48.0).clamp(0.0, max);
                self.scroll = next;
                KeyOutcome::Repaint
            }
            k::KEY_Page_Up => {
                let next = (self.scroll - 320.0).clamp(0.0, max);
                self.scroll = next;
                KeyOutcome::Repaint
            }
            k::KEY_Page_Down => {
                let next = (self.scroll + 320.0).clamp(0.0, max);
                self.scroll = next;
                KeyOutcome::Repaint
            }
            raw => {
                let utf8 = sym.key_char().map(String::from);
                let key = if ctrl { latin_sym.raw() } else { raw };
                match self.input.key(key, utf8.as_deref(), ctrl, shift) {
                    feather_ui::input::Edit::Changed => {
                        self.scroll = 0.0;
                        KeyOutcome::Repaint
                    }
                    feather_ui::input::Edit::Moved => KeyOutcome::Repaint,
                    feather_ui::input::Edit::Ignored => KeyOutcome::Ignored,
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &self,
        pm: &mut PixmapMut,
        fonts: &mut FontSystem,
        (cx, cy, cw, ch): (f32, f32, f32, f32),
        focused: bool,
        bindings: &Bindings,
        input_shortcut: &str,
    ) {
        let p = palette();
        let dark = appearance() == Appearance::Dark;
        let hc = high_contrast();

        let list_top = cy + HEADER_H;
        let list_bottom = cy + ch;
        let max_scroll = self.max_scroll(ch, bindings, input_shortcut);
        let scroll = self.scroll.clamp(0.0, max_scroll);
        let sections = self.filtered_sections(bindings, input_shortcut);

        // 1. Scrollable sections & keycap cards (drawn first so sticky header masks top edge cleanly)
        let card_x = cx + PAD_X;
        let card_w = (cw - PAD_X * 2.0).max(160.0);

        if sections.is_empty() {
            let mid_y = list_top + (ch - HEADER_H) * 0.42;
            fonts.draw_text_centered(
                pm,
                "Ничего не найдено",
                cx + cw * 0.5,
                mid_y - 12.0,
                15.0,
                p.text,
                true,
            );
            fonts.draw_text_centered(
                pm,
                "Попробуйте другой запрос или выберите вкладку «Все»",
                cx + cw * 0.5,
                mid_y + 12.0,
                12.5,
                p.text_secondary,
                false,
            );
        } else {
            let mut y = list_top + 14.0 - scroll;
            for (sec_idx, (cat, rows)) in sections.iter().enumerate() {
                if sec_idx > 0 {
                    y += SECTION_GAP;
                }
                let title_y = y;
                if title_y + SECTION_TITLE_H > list_top - 24.0 && title_y < list_bottom {
                    fonts.draw_text_clipped(
                        pm,
                        cat.section_title(),
                        card_x + 4.0,
                        title_y + 4.0,
                        card_w - 8.0,
                        12.5,
                        p.accent,
                        true,
                    );
                }
                y += SECTION_TITLE_H;

                let card_h = rows.len() as f32 * ROW_H;
                let card_bottom = y + card_h;
                if card_bottom > list_top && y < list_bottom {
                    // Clamp top of the card fill so a tall scrolled card never reaches above `cy`
                    let vis_top = y.max(list_top - 16.0);
                    let vis_bottom = card_bottom.min(list_bottom + 14.0);
                    let vis_h = (vis_bottom - vis_top).max(0.0);
                    if vis_h > 1.0 {
                        fill_rounded_rect(pm, card_x, vis_top, card_w, vis_h, 11.0, p.card);
                        stroke_rounded_rect(
                            pm,
                            card_x,
                            vis_top,
                            card_w,
                            vis_h,
                            11.0,
                            if hc { 1.0 } else { 0.6 },
                            p.glass_outline,
                        );
                    }

                    for (row_idx, (entry, resolved)) in rows.iter().enumerate() {
                        let ry = y + row_idx as f32 * ROW_H;
                        if ry + ROW_H <= list_top - 40.0 || ry >= list_bottom {
                            continue;
                        }
                        if row_idx > 0 && ry > list_top - 20.0 {
                            stroke_line(
                                pm,
                                card_x + 14.0,
                                ry,
                                card_x + card_w - 14.0,
                                ry,
                                if hc { 1.0 } else { 0.5 },
                                p.separator,
                            );
                        }
                        let caps_left = draw_keycaps_right(
                            pm,
                            fonts,
                            resolved,
                            card_x + card_w - 14.0,
                            ry + 11.0,
                            dark,
                            hc,
                        );
                        let text_x = card_x + 14.0;
                        let max_text_w = (caps_left - text_x - 14.0).max(80.0);
                        fonts.draw_text_clipped(
                            pm,
                            entry.title,
                            text_x,
                            ry + 6.0,
                            max_text_w,
                            13.0,
                            p.text,
                            true,
                        );
                        fonts.draw_text_clipped(
                            pm,
                            entry.desc,
                            text_x,
                            ry + 24.0,
                            max_text_w,
                            11.5,
                            p.text_secondary,
                            false,
                        );
                    }
                }
                y += card_h;
            }
        }

        // Scrollbar thumb when content overflows
        if max_scroll > 1.0 {
            let view_h = (ch - HEADER_H).max(60.0);
            let total_h = view_h + max_scroll;
            let track_top = list_top + 6.0;
            let track_h = (view_h - 14.0).max(24.0);
            let thumb_h = ((view_h / total_h) * track_h).clamp(24.0, track_h);
            let thumb_y = track_top + (scroll / max_scroll) * (track_h - thumb_h);
            fill_rounded_rect(
                pm,
                cx + cw - 7.5,
                thumb_y,
                3.5,
                thumb_h,
                1.75,
                if dark {
                    rgba(255, 255, 255, 78)
                } else {
                    rgba(0, 0, 0, 62)
                },
            );
        }

        // 2. Sticky header bar (cleanly covers scrolled rows under `cy..list_top`)
        fill_rounded_rect(pm, cx, cy, cw, HEADER_H, 0.0, p.window_bg);
        stroke_line(
            pm,
            cx,
            list_top - 0.5,
            cx + cw,
            list_top - 0.5,
            if hc { 1.0 } else { 0.6 },
            p.separator,
        );

        let g = self.header_geometry(fonts, cx, cy, cw);

        // Header title & subtitle on the left
        let title_max_w = (g.search_box.0 - (cx + PAD_X) - 12.0).max(120.0);
        fonts.draw_text_clipped(
            pm,
            "Горячие клавиши Пёрышка",
            cx + PAD_X,
            cy + 10.0,
            title_max_w,
            15.5,
            p.text,
            true,
        );
        fonts.draw_text_clipped(
            pm,
            "Работают в любой раскладке (RU / EN) · Печатайте для поиска",
            cx + PAD_X,
            cy + 30.5,
            title_max_w,
            11.0,
            p.text_secondary,
            false,
        );

        // Search input box
        let (sx, sy, sw, sh) = g.search_box;
        let search_bg = if dark {
            rgba(255, 255, 255, 16)
        } else {
            rgba(0, 0, 0, 11)
        };
        fill_rounded_rect(pm, sx, sy, sw, sh, sh * 0.5, search_bg);
        stroke_rounded_rect(
            pm,
            sx,
            sy,
            sw,
            sh,
            sh * 0.5,
            if focused { 1.0 } else { 0.6 },
            if focused && !self.input.is_empty() {
                p.accent
            } else {
                p.glass_outline
            },
        );
        // Magnifier loupe glyph
        let (lx, ly) = (sx + 13.5, sy + 13.0);
        stroke_circle(pm, lx, ly, 4.4, 1.35, p.text_secondary);
        stroke_line(
            pm,
            lx + 3.2,
            ly + 3.2,
            lx + 6.4,
            ly + 6.4,
            1.45,
            p.text_secondary,
        );

        if self.input.is_empty() {
            fonts.draw_text_clipped(
                pm,
                "Поиск клавиши или действия…",
                g.search_text_x,
                sy + 6.5,
                g.search_text_w,
                12.0,
                p.text_tertiary,
                false,
            );
            if focused {
                fill_rounded_rect(
                    pm,
                    g.search_text_x,
                    sy + 6.5,
                    1.5,
                    15.0,
                    0.75,
                    p.accent,
                );
            }
        } else {
            self.input.draw(
                pm,
                fonts,
                g.search_text_x,
                sy + 6.5,
                g.search_text_w,
                12.0,
                false,
                p.text,
                focused,
                p.accent,
            );
            if let Some((bx, by, bw, bh)) = g.clear_btn {
                let ccx = bx + bw * 0.5;
                let ccy = by + bh * 0.5;
                fill_circle(
                    pm,
                    ccx,
                    ccy,
                    7.5,
                    if dark {
                        rgba(255, 255, 255, 42)
                    } else {
                        rgba(0, 0, 0, 32)
                    },
                );
                let ink = if dark {
                    rgba(235, 238, 245, 230)
                } else {
                    rgba(255, 255, 255, 245)
                };
                stroke_line(pm, ccx - 2.6, ccy - 2.6, ccx + 2.6, ccy + 2.6, 1.35, ink);
                stroke_line(pm, ccx + 2.6, ccy - 2.6, ccx - 2.6, ccy + 2.6, 1.35, ink);
            }
        }

        // «Настроить…» button
        let (bx, by, bw, bh) = g.settings_btn;
        fill_rounded_rect(pm, bx, by, bw, bh, bh * 0.5, p.control_fill);
        stroke_rounded_rect(pm, bx, by, bw, bh, bh * 0.5, 0.7, p.glass_outline);
        fonts.draw_text_centered(
            pm,
            "Настроить…",
            bx + bw * 0.5,
            by + 6.5,
            12.0,
            p.text,
            true,
        );

        // Category pills row
        for (cat, (px, py, pw, ph)) in g.pills {
            let active = self.category == cat;
            if active {
                fill_rounded_rect(pm, px, py, pw, ph, ph * 0.5, p.accent);
                fonts.draw_text_centered(
                    pm,
                    cat.pill_label(),
                    px + pw * 0.5,
                    py + 5.0,
                    12.0,
                    p.on_accent,
                    true,
                );
            } else {
                fill_rounded_rect(pm, px, py, pw, ph, ph * 0.5, p.control_fill);
                stroke_rounded_rect(pm, px, py, pw, ph, ph * 0.5, 0.6, p.glass_outline);
                fonts.draw_text_centered(
                    pm,
                    cat.pill_label(),
                    px + pw * 0.5,
                    py + 5.0,
                    12.0,
                    p.text,
                    false,
                );
            }
        }
    }
}

/// Draws tactile 3D keycap badges right-aligned to `right_x` and returns the leftmost `x` used.
fn draw_keycaps_right(
    pm: &mut PixmapMut,
    fonts: &mut FontSystem,
    resolved: &ResolvedKeys,
    right_x: f32,
    y: f32,
    dark: bool,
    hc: bool,
) -> f32 {
    let p = palette();
    let cap_h = 22.0;
    match resolved {
        ResolvedKeys::Disabled => {
            let label = "Выключено";
            let w = (fonts.measure_text(label, 11.0, false) + 16.0).max(64.0);
            let x = right_x - w;
            fill_rounded_rect(pm, x, y, w, cap_h, 5.5, p.control_fill);
            fonts.draw_text_centered(
                pm,
                label,
                x + w * 0.5,
                y + 4.0,
                11.0,
                p.text_tertiary,
                false,
            );
            x
        }
        ResolvedKeys::Caps(caps) => {
            if caps.is_empty() {
                return right_x;
            }
            let plus_w = 12.0;
            let widths: Vec<f32> = caps
                .iter()
                .map(|c| (fonts.measure_text(c, 11.5, true) + 15.0).max(26.0))
                .collect();
            let total_w: f32 =
                widths.iter().sum::<f32>() + plus_w * (caps.len().saturating_sub(1) as f32);
            let start_x = right_x - total_w;

            let bevel_col = if dark {
                rgba(0, 0, 0, 105)
            } else {
                rgba(0, 0, 0, 28)
            };
            let face_col = if dark {
                rgba(60, 63, 72, 242)
            } else {
                rgba(255, 255, 255, 248)
            };

            let mut x = start_x;
            for (i, (cap, &w)) in caps.iter().zip(widths.iter()).enumerate() {
                if i > 0 {
                    fonts.draw_text_centered(
                        pm,
                        "+",
                        x + plus_w * 0.5,
                        y + 4.0,
                        10.5,
                        p.text_tertiary,
                        false,
                    );
                    x += plus_w;
                }
                // Subtle bottom 3D keycap edge + crisp top face
                fill_rounded_rect(pm, x, y + 1.2, w, cap_h, 5.5, bevel_col);
                fill_rounded_rect(pm, x, y, w, cap_h - 0.8, 5.5, face_col);
                stroke_rounded_rect(
                    pm,
                    x,
                    y,
                    w,
                    cap_h - 0.8,
                    5.5,
                    if hc { 1.1 } else { 0.7 },
                    p.glass_outline,
                );
                fonts.draw_text_centered(
                    pm,
                    cap,
                    x + w * 0.5,
                    y + 3.5,
                    11.5,
                    p.text,
                    true,
                );
                x += w;
            }
            start_x
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::input::keyboard::{keysyms as k, Keysym};

    #[test]
    fn search_matches_russian_english_keys_and_wrong_layout() {
        let mut guide = ShortcutsGuide::default();
        let bindings = Bindings::default();

        // Default shows all 5 sections
        assert_eq!(guide.filtered_sections(&bindings, "Alt+Shift").len(), 5);

        // Search by feature keyword
        guide.input.set("лента");
        let tape = guide.filtered_sections(&bindings, "Alt+Shift");
        assert!(!tape.is_empty());
        assert!(tape.iter().flat_map(|(_, r)| r).any(|(e, _)| e.title.contains("лент") || e.desc.contains("лент")));

        // Mistyped query in QWERTY ("ktynf" == "лента" on Russian keyboard)
        guide.input.set("ktynf");
        let flipped = guide.filtered_sections(&bindings, "Alt+Shift");
        assert_eq!(flipped.len(), tape.len());

        // Search by key chord token
        guide.input.set("print");
        let shots = guide.filtered_sections(&bindings, "Alt+Shift");
        assert!(shots.iter().flat_map(|(_, r)| r).count() >= 3);

        // Escape clears non-empty query first, then closes window on second press
        let first_esc = guide.on_key(
            Keysym::new(k::KEY_Escape),
            Keysym::new(k::KEY_Escape),
            false,
            false,
            560.0,
            &bindings,
            "Alt+Shift",
        );
        assert_eq!(first_esc, KeyOutcome::Repaint);
        assert!(guide.input.is_empty());

        let second_esc = guide.on_key(
            Keysym::new(k::KEY_Escape),
            Keysym::new(k::KEY_Escape),
            false,
            false,
            560.0,
            &bindings,
            "Alt+Shift",
        );
        assert_eq!(second_esc, KeyOutcome::CloseWindow);
    }

    #[test]
    fn custom_keybindings_reflect_live_in_guide() {
        use crate::shortcuts::Chord;
        let mut bindings = Bindings::default();
        bindings
            .set(
                ShortcutAction::Pip,
                Some(Chord {
                    mods: CTRL | ALT,
                    key: k::KEY_p,
                }),
            )
            .unwrap();
        match resolve_keys(KeySpec::Bound(ShortcutAction::Pip), &bindings, "Alt+Shift") {
            ResolvedKeys::Caps(caps) => assert_eq!(caps, vec!["Ctrl", "Alt", "P"]),
            ResolvedKeys::Disabled => panic!("expected bound caps"),
        }
        bindings.set(ShortcutAction::Pip, None).unwrap();
        assert!(matches!(
            resolve_keys(KeySpec::Bound(ShortcutAction::Pip), &bindings, "Alt+Shift"),
            ResolvedKeys::Disabled
        ));
    }
}
