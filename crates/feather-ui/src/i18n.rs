//! System language (`Lang`), regional formatting (`Region`), clock/date formatters, and
//! zero-allocation static/dynamic UI translation table for popular languages:
//! Russian (`ru`), English (`en`), Spanish (`es`), German (`de`), French (`fr`), and Chinese (`zh`).

use std::borrow::Cow;

/// Interface language of Feather DE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Lang {
    #[default]
    Ru = 0,
    En = 1,
    Es = 2,
    De = 3,
    Fr = 4,
    Zh = 5,
}

impl Lang {
    pub const ALL: [Self; 6] = [
        Self::Ru,
        Self::En,
        Self::Es,
        Self::De,
        Self::Fr,
        Self::Zh,
    ];

    pub const fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Ru,
            1 => Self::En,
            2 => Self::Es,
            3 => Self::De,
            4 => Self::Fr,
            5 => Self::Zh,
            _ => Self::Ru,
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Ru => "ru",
            Self::En => "en",
            Self::Es => "es",
            Self::De => "de",
            Self::Fr => "fr",
            Self::Zh => "zh",
        }
    }

    /// Native language name (never translated, so the user can always find their language).
    pub const fn native_name(self) -> &'static str {
        match self {
            Self::Ru => "Русский",
            Self::En => "English",
            Self::Es => "Español",
            Self::De => "Deutsch",
            Self::Fr => "Français",
            Self::Zh => "中文",
        }
    }

    pub const fn full_label(self) -> &'static str {
        match self {
            Self::Ru => "Русский — Russian",
            Self::En => "English — Английский",
            Self::Es => "Español — Испанский",
            Self::De => "Deutsch — Немецкий",
            Self::Fr => "Français — Французский",
            Self::Zh => "中文 (简体) — Китайский",
        }
    }

    pub const fn desktop_name_keys(self) -> &'static [&'static str] {
        match self {
            Self::Ru => &["Name[ru]=", "Name[ru_RU]="],
            Self::En => &["Name[en_US]=", "Name[en_GB]=", "Name[en]="],
            Self::Es => &["Name[es]=", "Name[es_ES]="],
            Self::De => &["Name[de]=", "Name[de_DE]="],
            Self::Fr => &["Name[fr]=", "Name[fr_FR]="],
            Self::Zh => &["Name[zh_CN]=", "Name[zh]=", "Name[zh_TW]="],
        }
    }

    pub const fn desktop_comment_keys(self) -> &'static [&'static str] {
        match self {
            Self::Ru => &["Comment[ru]=", "Comment[ru_RU]="],
            Self::En => &["Comment[en_US]=", "Comment[en_GB]=", "Comment[en]="],
            Self::Es => &["Comment[es]=", "Comment[es_ES]="],
            Self::De => &["Comment[de]=", "Comment[de_DE]="],
            Self::Fr => &["Comment[fr]=", "Comment[fr_FR]="],
            Self::Zh => &["Comment[zh_CN]=", "Comment[zh]=", "Comment[zh_TW]="],
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let lower = s.to_ascii_lowercase();
        let prefix = lower.split(['_', '.', '-', '@']).next().unwrap_or(&lower);
        match prefix {
            "ru" | "russian" => Some(Self::Ru),
            "en" | "english" => Some(Self::En),
            "es" | "spanish" | "espanol" => Some(Self::Es),
            "de" | "german" | "deutsch" => Some(Self::De),
            "fr" | "french" | "francais" => Some(Self::Fr),
            "zh" | "chinese" | "cn" => Some(Self::Zh),
            _ => None,
        }
    }
}

/// Regional format preset (date order, number separators, default units, POSIX locale).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Region {
    #[default]
    Ru = 0,
    Us = 1,
    Gb = 2,
    De = 3,
    Fr = 4,
    Es = 5,
    Cn = 6,
    Kz = 7,
}

impl Region {
    pub const ALL: [Self; 8] = [
        Self::Ru,
        Self::Us,
        Self::Gb,
        Self::De,
        Self::Fr,
        Self::Es,
        Self::Cn,
        Self::Kz,
    ];

    pub const fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Ru,
            1 => Self::Us,
            2 => Self::Gb,
            3 => Self::De,
            4 => Self::Fr,
            5 => Self::Es,
            6 => Self::Cn,
            7 => Self::Kz,
            _ => Self::Ru,
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Ru => "RU",
            Self::Us => "US",
            Self::Gb => "GB",
            Self::De => "DE",
            Self::Fr => "FR",
            Self::Es => "ES",
            Self::Cn => "CN",
            Self::Kz => "KZ",
        }
    }

    pub const fn locale_code(self) -> &'static str {
        match self {
            Self::Ru => "ru_RU.UTF-8",
            Self::Us => "en_US.UTF-8",
            Self::Gb => "en_GB.UTF-8",
            Self::De => "de_DE.UTF-8",
            Self::Fr => "fr_FR.UTF-8",
            Self::Es => "es_ES.UTF-8",
            Self::Cn => "zh_CN.UTF-8",
            Self::Kz => "ru_KZ.UTF-8",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Ru => "Россия",
            Self::Us => "США",
            Self::Gb => "Великобритания",
            Self::De => "Германия",
            Self::Fr => "Франция",
            Self::Es => "Испания",
            Self::Cn => "Китай",
            Self::Kz => "Казахстан",
        }
    }

    pub const fn step(self, delta: i32) -> Self {
        let len = Self::ALL.len() as i32;
        let idx = (self as i32 + delta).rem_euclid(len) as usize;
        Self::ALL[idx]
    }

    pub const fn sample_number(self) -> &'static str {
        match self {
            Self::Ru | Self::Fr | Self::Kz => "1 234,56",
            Self::De | Self::Es => "1.234,56",
            Self::Us | Self::Gb | Self::Cn => "1,234.56",
        }
    }

    pub fn format_preview(
        self,
        lang: Lang,
        use_24h: bool,
        first_monday: bool,
        metric: bool,
    ) -> String {
        let date = match (self, lang) {
            (_, Lang::Zh) | (Self::Cn, _) => "2026年10月25日",
            (Self::Us, _) => "Oct 25, 2026",
            (Self::De, _) => "25.10.2026",
            (Self::Gb | Self::Fr | Self::Es, _) => "25/10/2026",
            (Self::Ru | Self::Kz, _) => "25.10.2026",
        };
        let time = format_time_hm(14, 30, use_24h);
        let temp = if metric { "21 °C" } else { "70 °F" };
        let wk = match (lang, first_monday) {
            (Lang::Ru, true) => "Пн–Вс",
            (Lang::Ru, false) => "Вс–Сб",
            (Lang::En, true) => "Mon–Sun",
            (Lang::En, false) => "Sun–Sat",
            (Lang::Es, true) => "Lun–Dom",
            (Lang::Es, false) => "Dom–Sáb",
            (Lang::De, true) => "Mo–So",
            (Lang::De, false) => "So–Sa",
            (Lang::Fr, true) => "Lun–Dim",
            (Lang::Fr, false) => "Dim–Sam",
            (Lang::Zh, true) => "周一至周日",
            (Lang::Zh, false) => "周日至周六",
        };
        format!("{date}, {time} · {} · {temp} · {wk}", self.sample_number())
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let upper = s.to_ascii_uppercase();
        match upper.as_str() {
            "RU" | "RU_RU" | "RU_RU.UTF-8" => Some(Self::Ru),
            "US" | "EN_US" | "EN_US.UTF-8" => Some(Self::Us),
            "GB" | "UK" | "EN_GB" | "EN_GB.UTF-8" => Some(Self::Gb),
            "DE" | "DE_DE" | "DE_DE.UTF-8" => Some(Self::De),
            "FR" | "FR_FR" | "FR_FR.UTF-8" => Some(Self::Fr),
            "ES" | "ES_ES" | "ES_ES.UTF-8" => Some(Self::Es),
            "CN" | "ZH_CN" | "ZH_CN.UTF-8" => Some(Self::Cn),
            "KZ" | "RU_KZ" | "RU_KZ.UTF-8" => Some(Self::Kz),
            _ => None,
        }
    }
}

pub fn format_time_hm(hour: u32, minute: u32, use_24h: bool) -> String {
    if use_24h {
        format!("{hour}:{minute:02}")
    } else {
        let period = if hour < 12 { "AM" } else { "PM" };
        let h12 = match hour % 12 {
            0 => 12,
            h => h,
        };
        format!("{h12}:{minute:02} {period}")
    }
}

/// Top menu bar clock string (`weekday_mon0`: 0 = Monday … 6 = Sunday, `month0`: 0 = Jan … 11 = Dec).
pub fn format_topbar_clock(
    weekday_mon0: usize,
    day: u32,
    month0: usize,
    hour: u32,
    minute: u32,
) -> String {
    let lang = crate::scale::lang();
    let region = crate::scale::region();
    let use_24h = crate::scale::use_24h_time();
    let w = weekday_mon0.min(6);
    let m = month0.min(11);
    let time = format_time_hm(hour, minute, use_24h);
    match lang {
        Lang::Ru => {
            let weekday = ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"][w];
            let month = [
                "янв.", "февр.", "мар.", "апр.", "мая", "июн.", "июл.", "авг.", "сент.", "окт.",
                "нояб.", "дек.",
            ][m];
            format!("{weekday} {day} {month}  {time}")
        }
        Lang::En => {
            let weekday = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][w];
            let month = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ][m];
            if region == Region::Us {
                format!("{weekday} {month} {day}  {time}")
            } else {
                format!("{weekday} {day} {month}  {time}")
            }
        }
        Lang::Es => {
            let weekday = ["lun", "mar", "mié", "jue", "vie", "sáb", "dom"][w];
            let month = [
                "ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic",
            ][m];
            format!("{weekday} {day} {month}  {time}")
        }
        Lang::De => {
            let weekday = ["Mo.", "Di.", "Mi.", "Do.", "Fr.", "Sa.", "So."][w];
            let month = [
                "Jan.", "Feb.", "Mär.", "Apr.", "Mai", "Jun.", "Jul.", "Aug.", "Sep.", "Okt.",
                "Nov.", "Dez.",
            ][m];
            format!("{weekday} {day}. {month}  {time}")
        }
        Lang::Fr => {
            let weekday = ["lun.", "mar.", "mer.", "jeu.", "ven.", "sam.", "dim."][w];
            let month = [
                "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.",
                "nov.", "déc.",
            ][m];
            format!("{weekday} {day} {month}  {time}")
        }
        Lang::Zh => {
            let weekday = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"][w];
            format!("{}月{day}日 {weekday}  {time}", m + 1)
        }
    }
}

/// Lock screen date + time tuple.
pub fn format_lock_clock(
    weekday_mon0: usize,
    day: u32,
    month0: usize,
    hour: u32,
    minute: u32,
) -> (String, String) {
    let lang = crate::scale::lang();
    let region = crate::scale::region();
    let use_24h = crate::scale::use_24h_time();
    let w = weekday_mon0.min(6);
    let m = month0.min(11);
    let time = format_time_hm(hour, minute, use_24h);
    let date = match lang {
        Lang::Ru => {
            let weekday = [
                "понедельник",
                "вторник",
                "среда",
                "четверг",
                "пятница",
                "суббота",
                "воскресенье",
            ][w];
            let month = [
                "января",
                "февраля",
                "марта",
                "апреля",
                "мая",
                "июня",
                "июля",
                "августа",
                "сентября",
                "октября",
                "ноября",
                "декабря",
            ][m];
            format!("{weekday}, {day} {month}")
        }
        Lang::En => {
            let weekday = [
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
                "Sunday",
            ][w];
            let month = [
                "January",
                "February",
                "March",
                "April",
                "May",
                "June",
                "July",
                "August",
                "September",
                "October",
                "November",
                "December",
            ][m];
            if region == Region::Us {
                format!("{weekday}, {month} {day}")
            } else {
                format!("{weekday}, {day} {month}")
            }
        }
        Lang::Es => {
            let weekday = [
                "lunes",
                "martes",
                "miércoles",
                "jueves",
                "viernes",
                "sábado",
                "domingo",
            ][w];
            let month = [
                "enero",
                "febrero",
                "marzo",
                "abril",
                "mayo",
                "junio",
                "julio",
                "agosto",
                "septiembre",
                "octubre",
                "noviembre",
                "diciembre",
            ][m];
            format!("{weekday}, {day} de {month}")
        }
        Lang::De => {
            let weekday = [
                "Montag",
                "Dienstag",
                "Mittwoch",
                "Donnerstag",
                "Freitag",
                "Samstag",
                "Sonntag",
            ][w];
            let month = [
                "Januar",
                "Februar",
                "März",
                "April",
                "Mai",
                "Juni",
                "Juli",
                "August",
                "September",
                "Oktober",
                "November",
                "Dezember",
            ][m];
            format!("{weekday}, {day}. {month}")
        }
        Lang::Fr => {
            let weekday = [
                "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
            ][w];
            let month = [
                "janvier",
                "février",
                "mars",
                "avril",
                "mai",
                "juin",
                "juillet",
                "août",
                "septembre",
                "octobre",
                "novembre",
                "décembre",
            ][m];
            format!("{weekday} {day} {month}")
        }
        Lang::Zh => {
            let weekday = [
                "星期一", "星期二", "星期三", "星期四", "星期五", "星期六", "星期日",
            ][w];
            format!("{}月{day}日 {weekday}", m + 1)
        }
    };
    (date, time)
}

/// Translates a static Russian string literal into the active interface language.
#[inline]
pub fn tr_static(ru: &'static str) -> &'static str {
    let l = crate::scale::lang();
    if l == Lang::Ru {
        return ru;
    }
    lookup_exact(ru, l).unwrap_or(ru)
}

/// Translates a UI string into the active interface language.
/// Fast path: when `Lang::Ru` is active, returns `Cow::Borrowed(text)` immediately.
#[inline]
pub fn tr(text: &str) -> Cow<'_, str> {
    let l = crate::scale::lang();
    if l == Lang::Ru || text.is_empty() {
        return Cow::Borrowed(text);
    }
    if let Some(exact) = lookup_exact(text, l) {
        return Cow::Borrowed(exact);
    }
    if let Some(dyn_str) = translate_dynamic(text, l) {
        return Cow::Owned(dyn_str);
    }
    Cow::Borrowed(text)
}

/// Entry in the 5-language translation table: `(ru, en, es, de, fr, zh)`
type Entry = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

fn pick(e: &Entry, l: Lang) -> &'static str {
    match l {
        Lang::Ru => e.0,
        Lang::En => e.1,
        Lang::Es => e.2,
        Lang::De => e.3,
        Lang::Fr => e.4,
        Lang::Zh => e.5,
    }
}

static TABLE: &[Entry] = &[
    // ── Brand & Core Apps ──
    ("Пёрышко", "Feather", "Feather", "Feather", "Feather", "轻羽"),
    (
        "Добро пожаловать в Пёрышко",
        "Welcome to Feather",
        "Bienvenido a Feather",
        "Willkommen bei Feather",
        "Bienvenue dans Feather",
        "欢迎使用轻羽桌面",
    ),
    (
        "Alt+Tab — приложения • Super+Space — Spotlight • Alt+Shift — язык",
        "Alt+Tab — apps • Super+Space — Spotlight • Alt+Shift — input",
        "Alt+Tab — apps • Super+Space — Spotlight • Alt+Shift — idioma",
        "Alt+Tab — Apps • Super+Space — Spotlight • Alt+Shift — Eingabe",
        "Alt+Tab — apps • Super+Space — Spotlight • Alt+Shift — clavier",
        "Alt+Tab — 应用 • Super+Space — 搜索 • Alt+Shift — 输入法",
    ),
    ("Приложения", "Applications", "Aplicaciones", "Programme", "Applications", "应用程序"),
    ("Программы", "Applications", "Aplicaciones", "Programme", "Applications", "应用程序"),
    (
        "Все приложения сеткой (Super+A)",
        "All applications in a grid (Super+A)",
        "Todas las aplicaciones en cuadrícula (Super+A)",
        "Alle Programme im Raster (Super+A)",
        "Toutes les applications en grille (Super+A)",
        "网格显示所有应用 (Super+A)",
    ),
    ("Файлы", "Files", "Archivos", "Dateien", "Fichiers", "文件"),
    ("Файловый менеджер", "File Manager", "Gestor de archivos", "Dateimanager", "Gestionnaire de fichiers", "文件管理器"),
    ("Веб-браузер", "Web Browser", "Navegador web", "Webbrowser", "Navigateur web", "网页浏览器"),
    ("Терминал", "Terminal", "Terminal", "Terminal", "Terminal", "终端"),
    ("VPN-клиент", "VPN Client", "Cliente VPN", "VPN-Client", "Client VPN", "VPN 客户端"),
    ("Заметки", "Notes", "Notas", "Notizen", "Notes", "备忘录"),
    ("Заметки в стиле macOS", "macOS-style Notes", "Notas estilo macOS", "Notizen im macOS-Stil", "Notes style macOS", "macOS 风格备忘录"),
    ("Быстрые заметки (RU / EN)", "Quick Notes", "Notas rápidas", "Schnellnotizen", "Notes rapides", "快速备忘录"),
    (
        "Мониторинг системы",
        "Activity Monitor",
        "Monitor de actividad",
        "Aktivitätsanzeige",
        "Moniteur d’activité",
        "活动监视器",
    ),
    (
        "Память, окна Wayland и рендер",
        "Memory, Wayland windows and renderer",
        "Memoria, ventanas Wayland y renderizado",
        "Speicher, Wayland-Fenster und Renderer",
        "Mémoire, fenêtres Wayland et rendu",
        "内存、Wayland 窗口与渲染",
    ),
    (
        "Системные настройки",
        "System Settings",
        "Ajustes del Sistema",
        "Systemeinstellungen",
        "Réglages Système",
        "系统设置",
    ),
    (
        "Системные настройки…",
        "System Settings…",
        "Ajustes del Sistema…",
        "Systemeinstellungen…",
        "Réglages Système…",
        "系统设置…",
    ),
    (
        "Оформление, Dock, язык и масштаб",
        "Appearance, Dock, language and scale",
        "Apariencia, Dock, idioma y escala",
        "Erscheinungsbild, Dock, Sprache und Skalierung",
        "Apparence, Dock, langue et échelle",
        "外观、程序坞、语言与缩放",
    ),
    (
        "Об этом компьютере",
        "About This Computer",
        "Acerca de este equipo",
        "Über diesen Computer",
        "À propos de cet ordinateur",
        "关于本机",
    ),
    (
        "Пёрышко DE: версия и горячие клавиши",
        "Feather DE: version and shortcuts",
        "Feather DE: versión y atajos",
        "Feather DE: Version und Kurzbefehle",
        "Feather DE : version et raccourcis",
        "轻羽桌面：版本与快捷键",
    ),
    (
        "Перезагрузить оболочку",
        "Reload Shell",
        "Recargar interfaz",
        "Oberfläche neu laden",
        "Recharger l’interface",
        "重新加载外壳",
    ),
    (
        "Обновить интерфейс без закрытия окон (Super+Shift+R)",
        "Refresh UI without closing windows (Super+Shift+R)",
        "Actualizar interfaz sin cerrar ventanas (Super+Shift+R)",
        "Oberfläche ohne Fensterschließen neu laden (Super+Shift+R)",
        "Actualiser l’interface sans fermer les fenêtres (Super+Shift+R)",
        "不关闭窗口刷新界面 (Super+Shift+R)",
    ),
    ("Корзина", "Trash", "Papelera", "Papierkorb", "Corbeille", "废纸篓"),

    // ── Menu Bar & Window Menus ──
    ("Файл", "File", "Archivo", "Ablage", "Fichier", "文件"),
    ("Правка", "Edit", "Edición", "Bearbeiten", "Édition", "编辑"),
    ("Вид", "View", "Visualización", "Darstellung", "Présentation", "显示"),
    ("Переход", "Go", "Ir", "Gehe zu", "Aller", "前往"),
    ("Окно", "Window", "Ventana", "Fenster", "Fenêtre", "窗口"),
    ("Справка", "Help", "Ayuda", "Hilfe", "Aide", "帮助"),
    ("Свернуть все окна", "Minimize All Windows", "Minimizar todas las ventanas", "Alle Fenster minimieren", "Réduire toutes les fenêtres", "最小化所有窗口"),
    ("Выйти из Пёрышка…", "Log Out of Feather…", "Salir de Feather…", "Feather beenden…", "Quitter Feather…", "退出轻羽桌面…"),
    ("Скрыть остальные", "Hide Others", "Ocultar otros", "Andere ausblenden", "Masquer les autres", "隐藏其他"),
    ("Показать все", "Show All", "Mostrar todo", "Alle einblenden", "Tout afficher", "全部显示"),
    ("Удалить из Dock", "Remove from Dock", "Eliminar del Dock", "Aus dem Dock entfernen", "Supprimer du Dock", "从程序坞中移除"),
    ("Оставить в Dock", "Keep in Dock", "Mantener en el Dock", "Im Dock behalten", "Garder dans le Dock", "在程序坞中保留"),
    ("Свернуть", "Minimize", "Minimizar", "Minimieren", "Réduire", "最小化"),
    ("Увеличить", "Zoom", "Zoom", "Zoomen", "Réduire/agrandir", "缩放"),
    ("Войти в полноэкранный режим", "Enter Full Screen", "Usar pantalla completa", "Vollbildmodus ein", "Activer le mode plein écran", "进入全屏"),
    ("Закрыть окно", "Close Window", "Cerrar ventana", "Fenster schließen", "Fermer la fenêtre", "关闭窗口"),
    ("Закрыть", "Close", "Cerrar", "Schließen", "Fermer", "关闭"),
    ("Новое окно", "New Window", "Nueva ventana", "Neues Fenster", "Nouvelle fenêtre", "新建窗口"),
    ("Горячие клавиши Пёрышка", "Feather Keyboard Shortcuts", "Atajos de teclado de Feather", "Feather-Tastaturkurzbefehle", "Raccourcis clavier Feather", "轻羽快捷键"),

    // ── System Settings Sidebar Pages & Captions ──
    ("Оформление", "Appearance", "Apariencia", "Erscheinungsbild", "Apparence", "外观"),
    (
        "Выберите внешний вид и поведение интерфейса.",
        "Choose the look and behavior of the interface.",
        "Elige el aspecto y comportamiento de la interfaz.",
        "Wähle das Aussehen und Verhalten der Oberfläche.",
        "Choisissez l’apparence et le comportement de l’interface.",
        "选择界面的外观与行为方式。",
    ),
    (
        "Универсальный доступ",
        "Accessibility",
        "Accesibilidad",
        "Bedienungshilfen",
        "Accessibilité",
        "辅助功能",
    ),
    (
        "Крупный текст, контраст, прозрачность и залипание клавиш.",
        "Large text, contrast, transparency and sticky keys.",
        "Texto grande, contraste, transparencia y teclas fáciles.",
        "Großer Text, Kontrast, Transparenz und Einfingerbedienung.",
        "Grand texte, contraste, transparence et touches à auto-maintien.",
        "大号文本、对比度、透明度与粘滞键。",
    ),
    (
        "Язык и регион",
        "Language & Region",
        "Idioma y región",
        "Sprache & Region",
        "Langue et région",
        "语言与地区",
    ),
    (
        "Язык интерфейса, региональные форматы даты, времени и единиц.",
        "Interface language, regional date, time and unit formats.",
        "Idioma de la interfaz y formatos regionales de fecha, hora y unidades.",
        "Oberflächensprache sowie regionale Datums-, Zeit- und Einheitenformate.",
        "Langue de l’interface et formats régionaux de date, d’heure et d’unités.",
        "界面语言以及日期、时间和度量单位的区域格式。",
    ),
    ("Сеть", "Network", "Red", "Netzwerk", "Réseau", "网络"),
    (
        "Wi‑Fi, проводная сеть, VPN и сохранённые профили.",
        "Wi-Fi, Ethernet, VPN and saved profiles.",
        "Wi-Fi, red cableada, VPN y perfiles guardados.",
        "WLAN, Ethernet, VPN und gespeicherte Profile.",
        "Wi-Fi, réseau filaire, VPN et profils enregistrés.",
        "Wi-Fi、有线网络、VPN 与已保存配置文件。",
    ),
    ("Bluetooth", "Bluetooth", "Bluetooth", "Bluetooth", "Bluetooth", "蓝牙"),
    (
        "Беспроводные наушники, клавиатуры, мыши и другие устройства.",
        "Wireless headphones, keyboards, mice and other devices.",
        "Auriculares inalámbricos, teclados, ratones y otros dispositivos.",
        "Kabellose Kopfhörer, Tastaturen, Mäuse und andere Geräte.",
        "Écouteurs sans fil, claviers, souris et autres appareils.",
        "无线耳机、键盘、鼠标及其他设备。",
    ),
    ("Клавиатура", "Keyboard", "Teclado", "Tastatur", "Clavier", "键盘"),
    (
        "Источники ввода и переключение языка.",
        "Input sources and language switching.",
        "Fuentes de entrada y cambio de idioma.",
        "Eingabequellen und Sprachwechsel.",
        "Sources de saisie et changement de langue.",
        "输入法与语言切换。",
    ),
    ("Мышь и трекпад", "Mouse & Trackpad", "Ratón y trackpad", "Maus & Trackpad", "Souris et trackpad", "鼠标与触控板"),
    (
        "Курсор, скорость, прокрутка и нажатие.",
        "Cursor, pointer speed, scrolling and tap to click.",
        "Cursor, velocidad, desplazamiento y clic al tocar.",
        "Zeiger, Geschwindigkeit, Scrollen und Tippen zum Klicken.",
        "Curseur, vitesse, défilement et toucher pour cliquer.",
        "指针、速度、滚动与轻点点按。",
    ),
    ("Дисплеи", "Displays", "Pantallas", "Displays", "Écrans", "显示器"),
    (
        "Размер интерфейса, разрешение и частота экрана.",
        "Interface scale, resolution and refresh rate.",
        "Escala de interfaz, resolución y frecuencia de pantalla.",
        "Skalierung, Auflösung und Bildwiederholrate.",
        "Échelle de l’interface, résolution et taux de rafraîchissement.",
        "界面缩放、分辨率与刷新率。",
    ),
    (
        "Рабочий стол и Dock",
        "Desktop & Dock",
        "Escritorio y Dock",
        "Schreibtisch & Dock",
        "Bureau et Dock",
        "桌面与程序坞",
    ),
    (
        "Настройте рабочее пространство под себя.",
        "Customize your workspace and Dock.",
        "Personaliza tu espacio de trabajo y el Dock.",
        "Passe deinen Arbeitsbereich und das Dock an.",
        "Personnalisez votre espace de travail et le Dock.",
        "自定义工作空间与程序坞。",
    ),
    (
        "Дата и время",
        "Date & Time",
        "Fecha y hora",
        "Datum & Uhrzeit",
        "Date et heure",
        "日期与时间",
    ),
    (
        "Синхронизация часов и часовой пояс.",
        "Clock synchronization and time zone.",
        "Sincronización del reloj y zona horaria.",
        "Uhrzeitsynchronisierung und Zeitzone.",
        "Synchronisation de l’horloge et fuseau horaire.",
        "时钟同步与时区设置。",
    ),
    (
        "Экран блокировки и энергосбережение",
        "Lock Screen & Power",
        "Pantalla de bloqueo y energía",
        "Sperrbildschirm & Energie",
        "Écran verrouillé et économie d’énergie",
        "锁定屏幕与节能",
    ),
    (
        "Пароль, выключение экрана и переход в сон.",
        "Password, display sleep and system suspend.",
        "Contraseña, apagado de pantalla y reposo.",
        "Passwort, Ruhezustand für Display und System.",
        "Mot de passe, mise en veille de l’écran et du système.",
        "密码、关闭显示器与进入睡眠。",
    ),
    ("Звук", "Sound", "Sonido", "Ton", "Son", "声音"),
    (
        "Системные сигналы, устройства вывода и микрофоны.",
        "System alerts, output devices and microphones.",
        "Alertas del sistema, dispositivos de salida y micrófonos.",
        "Systemtöne, Ausgabegeräte und Mikrofone.",
        "Sons système, périphériques de sortie et microphones.",
        "系统提示音、输出设备与麦克风。",
    ),
    ("Уведомления", "Notifications", "Notificaciones", "Mitteilungen", "Notifications", "通知"),
    (
        "Баннеры, центр уведомлений и «Не беспокоить».",
        "Banners, Notification Center and Do Not Disturb.",
        "Tiras, centro de notificaciones y No molestar.",
        "Banner, Mitteilungszentrale und Nicht stören.",
        "Bannières, centre de notifications et Ne pas déranger.",
        "横幅、通知中心与勿扰模式。",
    ),
    (
        "Приложения по умолчанию и автозапуск при входе.",
        "Default applications and login autostart.",
        "Aplicaciones por omisión e inicio automático.",
        "Standardprogramme und Anmeldeobjekte.",
        "Applications par défaut et ouverture au démarrage.",
        "默认应用程序与登录启动项。",
    ),

    // ── Language & Region Page ──
    ("Язык интерфейса", "Interface Language", "Idioma de la interfaz", "Oberflächensprache", "Langue de l’interface", "界面语言"),
    ("Основной язык", "Primary Language", "Idioma principal", "Primäre Sprache", "Langue principale", "首选语言"),
    ("Русский — Russian", "Russian — Русский", "Ruso — Русский", "Russisch — Русский", "Russe — Русский", "俄语 — Русский"),
    ("English — Английский", "English — United States / UK", "Inglés — English", "Englisch — English", "Anglais — English", "英语 — English"),
    ("Español — Испанский", "Spanish — Español", "Español — España / Latinoamérica", "Spanisch — Español", "Espagnol — Español", "西班牙语 — Español"),
    ("Deutsch — Немецкий", "German — Deutsch", "Alemán — Deutsch", "Deutsch — Deutschland / Österreich", "Allemand — Deutsch", "德语 — Deutsch"),
    ("Français — Французский", "French — Français", "Francés — Français", "Französisch — Français", "Français — France / Canada", "法语 — Français"),
    ("中文 (简体) — Китайский", "Chinese (Simplified) — 中文", "Chino simplificado — 中文", "Chinesisch (Vereinfacht) — 中文", "Chinois simplifié — 中文", "简体中文 — Chinese"),
    (
        "Применяется сразу к панелям, меню, Настройкам, Файлам и Заметкам",
        "Applies immediately to bars, menus, Settings, Files and Notes",
        "Se aplica al instante a barras, menús, Ajustes, Archivos y Notas",
        "Gilt sofort für Leisten, Menüs, Einstellungen, Dateien und Notizen",
        "S’applique aussitôt aux barres, menus, Réglages, Fichiers et Notes",
        "立即应用于菜单栏、菜单、设置、文件和备忘录",
    ),
    (
        "Применяется сразу к строке меню, Dock, Центру управления, Настройкам, Файлам и Заметкам.",
        "Applies immediately to the menu bar, Dock, Control Center, Settings, Files and Notes.",
        "Se aplica al instante a la barra de menús, Dock, Centro de control, Ajustes, Archivos y Notas.",
        "Gilt sofort für Menüleiste, Dock, Kontrollzentrum, Einstellungen, Dateien und Notizen.",
        "S’applique aussitôt à la barre des menus, au Dock, au Centre de contrôle, aux Réglages, Fichiers et Notes.",
        "立即应用于菜单栏、程序坞、控制中心、设置、文件和备忘录。",
    ),
    ("Регион и форматы", "Region & Formats", "Región y formatos", "Region & Formate", "Région et formats", "地区与格式"),
    ("Регион", "Region", "Región", "Region", "Région", "地区"),
    ("Россия", "Russia", "Rusia", "Russland", "Russie", "俄罗斯"),
    ("США", "United States", "Estados Unidos", "Vereinigte Staaten", "États-Unis", "美国"),
    ("Великобритания", "United Kingdom", "Reino Unido", "Vereinigtes Königreich", "Royaume-Uni", "英国"),
    ("Германия", "Germany", "Alemania", "Deutschland", "Allemagne", "德国"),
    ("Франция", "France", "Francia", "Frankreich", "France", "法国"),
    ("Испания", "Spain", "España", "Spanien", "Espagne", "西班牙"),
    ("Китай", "China", "China", "China", "Chine", "中国"),
    ("Казахстан", "Kazakhstan", "Kazajistán", "Kasachstan", "Kazakhstan", "哈萨克斯坦"),
    ("24‑часовой формат времени", "24-Hour Time", "Formato de 24 horas", "24-Stunden-Format", "Format 24 heures", "24 小时制"),
    (
        "Часы в строке меню и на экране блокировки",
        "Clock in the menu bar and on the lock screen",
        "Reloj en la barra de menús y en la pantalla de bloqueo",
        "Uhr in der Menüleiste und auf dem Sperrbildschirm",
        "Horloge dans la barre des menus et sur l’écran verrouillé",
        "菜单栏和锁定屏幕上的时钟格式",
    ),
    (
        "Показывать 14:30 вместо 2:30 PM в строке меню и на экране блокировки",
        "Show 14:30 instead of 2:30 PM in the menu bar and lock screen",
        "Mostrar 14:30 en lugar de 2:30 PM en la barra de menús y bloqueo",
        "14:30 statt 2:30 PM in Menüleiste und Sperrbildschirm anzeigen",
        "Afficher 14:30 au lieu de 2:30 PM dans la barre des menus",
        "在菜单栏和锁定屏幕中显示 14:30 而非 2:30 PM",
    ),
    ("Первый день недели", "First Day of Week", "Primer día de la semana", "Erster Wochentag", "Premier jour de la semaine", "每周第一天"),
    ("Понедельник", "Monday", "Lunes", "Montag", "Lundi", "星期一"),
    ("Воскресенье", "Sunday", "Domingo", "Sonntag", "Dimanche", "星期日"),
    ("Единицы измерения", "Measurement Units", "Unidades de medida", "Maßeinheiten", "Unités de mesure", "度量单位"),
    ("Метрические", "Metric", "Métricas", "Metrisch", "Métriques", "公制"),
    ("Имперские", "Imperial", "Imperiales", "Imperial", "Impériales", "英制"),
    ("США (дюймы, °F)", "US (inches, °F)", "EE. UU. (pulg., °F)", "US (Zoll, °F)", "US (pouces, °F)", "美制 (英寸, °F)"),
    ("Метрические (°C, км)", "Metric (°C, km)", "Métricas (°C, km)", "Metrisch (°C, km)", "Métriques (°C, km)", "公制 (°C, km)"),
    ("Имперские (°F, мили)", "Imperial (°F, mi)", "Imperiales (°F, mi)", "Imperial (°F, mi)", "Impériales (°F, mi)", "英制 (°F, mi)"),
    ("Пример форматирования", "Formatting Example", "Ejemplo de formato", "Formatbeispiel", "Exemple de format", "格式示例"),
    (
        "Числа, валюта, дата и температура",
        "Numbers, currency, date and temperature",
        "Números, moneda, fecha y temperatura",
        "Zahlen, Währung, Datum und Temperatur",
        "Nombres, devise, date et température",
        "数字、货币、日期和温度",
    ),
    (
        "Дата, время, числа и температура для выбранного региона",
        "Date, time, numbers and temperature for the selected region",
        "Fecha, hora, números y temperatura para la región elegida",
        "Datum, Uhrzeit, Zahlen und Temperatur für die gewählte Region",
        "Date, heure, nombres et température pour la région choisie",
        "所选地区的日期、时间、数字和温度格式",
    ),
    (
        "Язык и региональные стандарты также передаются программам, запускаемым в сессии Пёрышка.",
        "Language and regional standards are also passed to apps launched in the Feather session.",
        "El idioma y el formato regional también se aplican a las apps abiertas en Feather.",
        "Sprache und Regionalformate werden auch an gestartete Programme übergeben.",
        "La langue et les formats régionaux sont aussi transmis aux applications lancées.",
        "语言和区域格式也会传递给在轻羽桌面会话中启动的应用程序。",
    ),
    ("сейчас", "now", "ahora", "jetzt", "maintenant", "刚刚"),
    ("Блокировка и питание", "Lock Screen & Power", "Bloqueo y energía", "Sperre & Energie", "Verrouillage et énergie", "锁定与电源"),
    ("Как цвет акцента", "Accent Color", "Color de acento", "Akzentfarbe", "Couleur d’accentuation", "强调色"),
    ("Анимации интерфейса", "Interface Animations", "Animaciones de la interfaz", "Oberflächenanimationen", "Animations de l’interface", "界面动画"),
    ("Появление окон, меню и запуск приложений", "Window transitions, menus and app launches", "Aparición de ventanas, menús y apertura de apps", "Fensterübergänge, Menüs und Programmstart", "Ouverture des fenêtres, menus et lancement d’apps", "窗口过渡、菜单与启动应用动画"),
    ("Программы GTK в стиле Пёрышка", "Feather Style for GTK Apps", "Estilo Feather para apps GTK", "Feather-Stil für GTK-Programme", "Style Feather pour les apps GTK", "GTK 应用使用轻羽样式"),
    ("Светофоры и заголовки в Firefox и программах GTK 3/4", "Traffic lights and titlebars in Firefox and GTK 3/4 apps", "Botones semáforo y barras en Firefox y apps GTK 3/4", "Ampeltasten und Titelleisten in Firefox und GTK 3/4", "Boutons tricolores dans Firefox et les apps GTK 3/4", "在 Firefox 和 GTK 3/4 应用中使用红黄绿按钮"),
    ("Цвета и размер текста сразу применяются к системным панелям, Файлам и Заметкам.", "Colors and text size apply immediately to system panels, Files and Notes.", "Los colores y el tamaño de texto se aplican al instante a paneles, Archivos y Notas.", "Farben und Textgröße gelten sofort für Systemleisten, Dateien und Notizen.", "Les couleurs et la taille du texte s’appliquent aussitôt aux panneaux, Fichiers et Notes.", "颜色和文字大小会立即应用于系统面板、文件和备忘录。"),
    ("Чёткие границы карточек, панелей, разделителей и подписей", "Sharper borders for cards, panels, dividers and labels", "Bordes nítidos en tarjetas, paneles, divisores y etiquetas", "Klare Ränder für Karten, Leisten, Trennlinien und Beschriftungen", "Bordures nettes pour les cartes, panneaux, séparateurs et libellés", "卡片、面板、分隔线和标签使用更清晰的边框"),
    ("Сплошной непрозрачный фон меню, Dock, Центра управления и окон", "Solid opaque background for menus, Dock, Control Center and windows", "Fondo opaco sólido en menús, Dock, Centro de control y ventanas", "Deckender Hintergrund für Menüs, Dock, Kontrollzentrum und Fenster", "Fond opaque uni pour les menus, le Dock, le Centre de contrôle et les fenêtres", "菜单、程序坞、控制中心和窗口使用纯色不透明背景"),
    ("Позволяет нажимать сочетания клавиш последовательно. Повторное нажатие модификатора снимает фиксацию.", "Allows pressing key chords sequentially. Pressing a modifier again unlatches it.", "Permite pulsar atajos tecla por tecla. Pulsar el modificador otra vez lo libera.", "Erlaubt das Nacheinanderdrücken von Tastenkombinationen.", "Permet d’appuyer sur les raccourcis touche par touche.", "允许依次按下组合键。再次按下修饰键可取消锁定。"),
    ("Источники ввода", "Input Sources", "Fuentes de entrada", "Eingabequellen", "Sources de saisie", "输入法"),
    ("Сочетания клавиш", "Keyboard Shortcuts", "Atajos de teclado", "Tastaturkurzbefehle", "Raccourcis clavier", "键盘快捷键"),

    // ── Appearance & Accessibility ──
    ("Светлое", "Light", "Claro", "Hell", "Clair", "浅色"),
    ("Тёмное", "Dark", "Oscuro", "Dunkel", "Sombre", "深色"),
    ("Светлый режим", "Light Mode", "Modo claro", "Heller Modus", "Mode clair", "浅色模式"),
    ("Тёмный режим", "Dark Mode", "Modo oscuro", "Dunkler Modus", "Mode sombre", "深色模式"),
    ("Тема оформления", "Appearance Theme", "Tema de apariencia", "Erscheinungsbild", "Thème d’apparence", "外观主题"),
    ("Цвета и текст", "Colors & Text", "Colores y texto", "Farben & Text", "Couleurs et texte", "颜色与文本"),
    ("Цвет акцента", "Accent Color", "Color de acento", "Akzentfarbe", "Couleur d’accentuation", "强调色"),
    ("Кнопки, переключатели и выбранные элементы", "Buttons, switches and active controls", "Botones, interruptores y controles activos", "Tasten, Schalter und aktive Elemente", "Boutons, commutateurs et éléments actifs", "按钮、开关与选中控件"),
    ("Цвет выделения", "Highlight Color", "Color de realce", "Auswahlfarbe", "Couleur de surbrillance", "高亮颜色"),
    ("Выделение текста в полях ввода и заметках", "Text selection in input fields and notes", "Selección de texto en campos y notas", "Textauswahl in Eingabefeldern und Notizen", "Sélection de texte dans les champs et notes", "输入框与备忘录中的文本选区"),
    ("Размер текста", "Text Size", "Tamaño del texto", "Textgröße", "Taille du texte", "文本大小"),
    ("Размер текста интерфейса", "Interface Text Size", "Tamaño del texto de la interfaz", "Textgröße der Oberfläche", "Taille du texte de l’interface", "界面文字大小"),
    ("Мелкий", "Small", "Pequeño", "Klein", "Petit", "小"),
    ("Обычный", "Default", "Estándar", "Standard", "Normal", "默认"),
    ("Крупный", "Large", "Grande", "Groß", "Grand", "大"),
    ("Очень крупный", "Extra Large", "Muy grande", "Sehr groß", "Très grand", "特大"),
    ("Макс.", "Max", "Máx.", "Max.", "Max.", "最大"),
    ("Синий", "Blue", "Azul", "Blau", "Bleu", "蓝色"),
    ("Фиолетовый", "Purple", "Morado", "Lila", "Violet", "紫色"),
    ("Розовый", "Pink", "Rosa", "Rosa", "Rose", "粉色"),
    ("Красный", "Red", "Rojo", "Rot", "Rouge", "红色"),
    ("Оранжевый", "Orange", "Naranja", "Orange", "Orange", "橙色"),
    ("Жёлтый", "Yellow", "Amarillo", "Gelb", "Jaune", "黄色"),
    ("Зелёный", "Green", "Verde", "Grün", "Vert", "绿色"),
    ("Графитовый", "Graphite", "Grafito", "Graphit", "Graphite", "石墨色"),
    ("Плавные анимации", "Smooth Animations", "Animaciones fluidas", "Flüssige Animationen", "Animations fluides", "平滑动画"),
    ("Пружинная физика открытия, сворачивания и переключения окон", "Spring physics when opening, minimizing and switching windows", "Física de resorte al abrir, minimizar y cambiar ventanas", "Federphysik beim Öffnen, Minimieren und Wechseln", "Physique de ressort à l’ouverture et réduction des fenêtres", "打开、最小化和切换窗口时的弹簧动画"),
    ("Сторонние приложения", "Third-Party Apps", "Aplicaciones de terceros", "Drittanbieter-Apps", "Applications tierces", "第三方应用"),
    ("Оформление программ GTK и Qt", "GTK & Qt App Styling", "Estilo de aplicaciones GTK y Qt", "GTK- & Qt-Darstellung", "Style des applications GTK et Qt", "GTK 与 Qt 应用样式"),
    ("Записать тему Tahoe для GTK 3/4, libadwaita и Qt", "Write Tahoe theme for GTK 3/4, libadwaita and Qt", "Escribir tema Tahoe para GTK 3/4, libadwaita y Qt", "Tahoe-Thema für GTK 3/4, libadwaita und Qt schreiben", "Appliquer le thème Tahoe à GTK 3/4, libadwaita et Qt", "为 GTK 3/4、libadwaita 和 Qt 写入 Tahoe 主题"),
    ("Применить", "Apply", "Aplicar", "Anwenden", "Appliquer", "应用"),
    ("Зрение", "Vision", "Visión", "Sehen", "Vision", "视觉"),
    ("Увеличить контраст", "Increase Contrast", "Aumentar contraste", "Kontrast erhöhen", "Augmenter le contraste", "增加对比度"),
    ("Более чёткие границы панелей, карточек, кнопок и текста", "Sharper borders on panels, cards, buttons and text", "Bordes más definidos en paneles, tarjetas, botones y texto", "Schärfere Ränder für Leisten, Karten, Tasten und Text", "Bordures plus nettes sur les panneaux, cartes, boutons et textes", "更清晰的面板、卡片、按钮与文本边框"),
    ("Уменьшить прозрачность", "Reduce Transparency", "Reducir transparencia", "Transparenz reduzieren", "Réduire la transparence", "降低透明度"),
    ("Заменить размытое стекло плотным непрозрачным фоном", "Replace blurred glass with solid opaque backgrounds", "Sustituir el cristal translúcido por fondos opacos", "Verschwommenes Glas durch deckenden Hintergrund ersetzen", "Remplacer le verre dépoli par des fonds opaques", "用纯色不透明背景替代毛玻璃效果"),
    ("Применяется сразу к панелям, окнам, Файлам и Заметкам.", "Applies immediately to panels, windows, Files and Notes.", "Se aplica al instante a paneles, ventanas, Archivos y Notas.", "Gilt sofort für Leisten, Fenster, Dateien und Notizen.", "S’applique aussitôt aux panneaux, fenêtres, Fichiers et Notes.", "立即应用于面板、窗口、文件和备忘录。"),
    ("Клавиатура и ввод", "Keyboard & Input", "Teclado y entrada", "Tastatur & Eingabe", "Clavier et saisie", "键盘与输入"),
    ("Залипание клавиш", "Sticky Keys", "Teclas fáciles", "Einfingerbedienung", "Touches à auto-maintien", "粘滞键"),
    ("Shift, Ctrl, Alt и Super удерживаются для следующей клавиши", "Shift, Ctrl, Alt and Super stay latched for the next key", "Shift, Ctrl, Alt y Super se mantienen para la siguiente tecla", "Shift, Strg, Alt und Super bleiben für die nächste Taste aktiv", "Shift, Ctrl, Alt et Super restent actifs pour la touche suivante", "单按 Shift、Ctrl、Alt 和 Super 键将保持到下一次按键"),

    // ── Common Controls & Status ──
    ("Включено", "On", "Activado", "Ein", "Activé", "已开启"),
    ("Выключено", "Off", "Desactivado", "Aus", "Désactivé", "已关闭"),
    ("Вкл", "On", "Act.", "Ein", "Act.", "开"),
    ("Выкл", "Off", "Des.", "Aus", "Dés.", "关"),
    ("Включить", "Turn On", "Activar", "Einschalten", "Activer", "开启"),
    ("Выключить", "Turn Off", "Desactivar", "Ausschalten", "Désactiver", "关闭"),
    ("Подключено", "Connected", "Conectado", "Verbunden", "Connecté", "已连接"),
    ("Отключено", "Disconnected", "Desconectado", "Getrennt", "Déconnecté", "未连接"),
    ("Подключить", "Connect", "Conectar", "Verbinden", "Connecter", "连接"),
    ("Отключить", "Disconnect", "Desconectar", "Trennen", "Déconnecter", "断开"),
    ("Забыть", "Forget", "Olvidar", "Ignorieren", "Oublier", "忽略此网络"),
    ("Поиск…", "Searching…", "Buscando…", "Suchen…", "Recherche…", "正在搜索…"),
    ("Обновить", "Refresh", "Actualizar", "Aktualisieren", "Actualiser", "刷新"),
    ("Отмена", "Cancel", "Cancelar", "Abbrechen", "Annuler", "取消"),
    ("Сохранить", "Save", "Guardar", "Sichern", "Enregistrer", "保存"),
    ("Выбрать", "Choose", "Seleccionar", "Auswählen", "Choisir", "选择"),
    ("Добавить", "Add", "Añadir", "Hinzufügen", "Ajouter", "添加"),
    ("Добавить…", "Add…", "Añadir…", "Hinzufügen…", "Ajouter…", "添加…"),
    ("Удалить", "Remove", "Eliminar", "Entfernen", "Supprimer", "移除"),
    ("Сбросить", "Reset", "Restablecer", "Zurücksetzen", "Réinitialiser", "重置"),
    ("Готово", "Done", "Aceptar", "Fertig", "Terminé", "完成"),
    ("Изменить…", "Change…", "Cambiar…", "Ändern…", "Modifier…", "更改…"),
    ("Открыть", "Open", "Abrir", "Öffnen", "Ouvrir", "打开"),

    // ── Control Center & Notifications ──
    ("Не беспокоить", "Do Not Disturb", "No molestar", "Nicht stören", "Ne pas déranger", "勿扰模式"),
    ("Фокус", "Focus", "Concentración", "Fokus", "Concentration", "专注模式"),
    ("По расписанию", "Scheduled", "Programado", "Geplant", "Programmé", "定时"),
    ("Дисплей", "Display", "Pantalla", "Display", "Écran", "显示器"),
    ("Яркость", "Brightness", "Brillo", "Helligkeit", "Luminosité", "亮度"),
    ("Громкость", "Volume", "Volumen", "Lautstärke", "Volume", "音量"),
    ("Масштаб", "Scale", "Escala", "Skalierung", "Échelle", "缩放"),
    ("Обои", "Wallpaper", "Fondo de pantalla", "Hintergrundbild", "Fond d’écran", "壁纸"),
    ("Раскладка", "Input Source", "Teclado", "Eingabequelle", "Clavier", "输入法"),
    ("Настройки сети…", "Network Settings…", "Ajustes de red…", "Netzwerkeinstellungen…", "Réglages Réseau…", "网络设置…"),
    ("Настройки Bluetooth…", "Bluetooth Settings…", "Ajustes de Bluetooth…", "Bluetooth-Einstellungen…", "Réglages Bluetooth…", "蓝牙设置…"),
    ("Настройки звука…", "Sound Settings…", "Ajustes de sonido…", "Toneinstellungen…", "Réglages Son…", "声音设置…"),
    ("Очистить все", "Clear All", "Borrar todo", "Alle löschen", "Tout effacer", "全部清除"),
    ("Нет новых уведомлений", "No New Notifications", "No hay notificaciones nuevas", "Keine neuen Mitteilungen", "Aucune nouvelle notification", "没有新通知"),
    ("Поиск в Пёрышке…", "Spotlight Search…", "Búsqueda en Feather…", "Feather-Suche…", "Recherche Feather…", "聚焦搜索…"),
    ("Поиск", "Search", "Buscar", "Suchen", "Rechercher", "搜索"),
    ("Ничего не найдено", "No Results Found", "Sin resultados", "Keine Treffer", "Aucun résultat", "未找到结果"),

    // ── Desktop, Dock & Displays ──
    ("Настройте рабочее пространство под себя.", "Customize your workspace, Dock, screenshots and hot corners.", "Personaliza tu espacio de trabajo, Dock y capturas.", "Passen Sie Arbeitsbereich, Dock und Bildschirmfotos an.", "Personnalisez votre espace de travail, le Dock et les captures.", "自定义工作区、程序坞、屏幕快照和触发角。"),
    ("Текущие обои", "Current Wallpaper", "Fondo actual", "Aktuelles Hintergrundbild", "Fond d’écran actuel", "当前壁纸"),
    ("Рассвет", "Dawn", "Amanecer", "Morgendämmerung", "Aube", "黎明"),
    ("Сияние", "Aurora", "Aurora", "Polarlicht", "Aurore", "极光"),
    ("Горное озеро", "Mountain Lake", "Lago de montaña", "Bergsee", "Lac de montagne", "高山湖泊"),
    ("Выбрать файл…", "Choose File…", "Seleccionar archivo…", "Datei auswählen…", "Choisir un fichier…", "选择文件…"),
    ("Выбор…", "Choosing…", "Seleccionando…", "Auswahl…", "Sélection…", "正在选择…"),
    ("Свои изображения PNG, JPEG и WebP заполняют экран без искажения. Повторный выбор градиента переключает его вариант.", "Custom PNG, JPEG and WebP images fill the screen without distortion. Clicking the gradient cycles its style.", "Las imágenes PNG, JPEG y WebP llenan la pantalla sin distorsión. Pulsar el degradado cambia su variante.", "Eigene PNG-, JPEG- und WebP-Bilder füllen den Bildschirm verzerrungsfrei.", "Les images PNG, JPEG et WebP remplissent l’écran sans déformation.", "自定义 PNG、JPEG 和 WebP 图像无失真填充屏幕。再次点击渐变可切换样式。"),
    ("Увеличение значков", "Magnification", "Ampliación de iconos", "Vergrößerung", "Agrandissement", "图标放大"),
    ("Значки увеличиваются при наведении", "Icons magnify when hovering with the pointer", "Los iconos se amplían al pasar el cursor", "Symbole vergrößern sich beim Zeigen", "Les icônes s’agrandissent au survol", "指针悬停时放大图标"),
    ("Размер значков", "Icon Size", "Tamaño de iconos", "Symbolgröße", "Taille des icônes", "图标大小"),
    ("Маленькие", "Small", "Pequeños", "Klein", "Petites", "小"),
    ("Средние", "Medium", "Medianos", "Mittel", "Moyennes", "中"),
    ("Крупные", "Large", "Grandes", "Groß", "Grandes", "大"),
    ("Автоматически скрывать Dock", "Automatically Hide the Dock", "Ocultar el Dock automáticamente", "Dock automatisch ausblenden", "Masquer automatiquement le Dock", "自动隐藏程序坞"),
    ("Появляется, когда указатель у нижнего края", "Appears when the pointer approaches the edge", "Aparece al acercar el cursor al borde", "Erscheint, wenn der Zeiger den Rand berührt", "Apparaît lorsque le pointeur approche du bord", "当指针移近屏幕边缘时显示"),
    ("Снимки экрана", "Screenshots", "Capturas de pantalla", "Bildschirmfotos", "Captures d’écran", "屏幕快照"),
    ("Папка сохранения", "Save Location", "Carpeta de guardado", "Speicherort", "Dossier d’enregistrement", "保存位置"),
    ("Изображения / Снимки экрана", "Pictures / Screenshots", "Imágenes / Capturas de pantalla", "Bilder / Bildschirmfotos", "Images / Captures d’écran", "图片 / 屏幕快照"),
    ("По умолчанию", "Default", "Por omisión", "Standard", "Par défaut", "默认"),
    ("Выбрать папку…", "Choose Folder…", "Seleccionar carpeta…", "Ordner auswählen…", "Choisir un dossier…", "选择文件夹…"),
    ("Выбрать папку", "Choose Folder", "Seleccionar carpeta", "Ordner auswählen", "Choisir le dossier", "选择文件夹"),
    ("Папка для снимков экрана", "Screenshots Folder", "Carpeta para capturas de pantalla", "Ordner für Bildschirmfotos", "Dossier des captures d’écran", "屏幕快照文件夹"),
    ("Формат файлов", "File Format", "Formato de archivo", "Dateiformat", "Format de fichier", "文件格式"),
    ("Расширение .png", "Extension .png", "Extensión .png", "Endung .png", "Extension .png", "扩展名 .png"),
    ("Расширение .jpg", "Extension .jpg", "Extensión .jpg", "Endung .jpg", "Extension .jpg", "扩展名 .jpg"),
    ("Расширение .webp", "Extension .webp", "Extensión .webp", "Endung .webp", "Extension .webp", "扩展名 .webp"),
    ("Print — весь экран, Shift+Print — область или окно, Alt+Print — активное окно. Снимки также копируются в буфер обмена.", "Print — full screen, Shift+Print — region or window, Alt+Print — active window. Screenshots are also copied to the clipboard.", "Print — pantalla completa, Shift+Print — área o ventana, Alt+Print — ventana activa. También se copian al portapapeles.", "Print — ganzer Bildschirm, Shift+Print — Bereich oder Fenster, Alt+Print — aktives Fenster. Wird auch in die Zwischenablage kopiert.", "Print — plein écran, Shift+Print — zone ou fenêtre, Alt+Print — fenêtre active. Également copié dans le presse-papiers.", "Print — 全屏，Shift+Print — 区域或窗口，Alt+Print — 活动窗口。屏幕快照同时复制到剪贴板。"),
    ("Подведите указатель в угол экрана, чтобы выполнить действие. Нажмите на строку, чтобы выбрать его.", "Move the pointer to a screen corner to trigger its action. Click a row to change it.", "Lleva el cursor a una esquina para activar su acción. Pulsa una fila para cambiarla.", "Bewegen Sie den Zeiger in eine Bildschirmecke, um die Aktion auszulösen.", "Placez le pointeur dans un coin de l’écran pour déclencher l’action.", "将指针移至屏幕角落以触发操作。点击行可更改动作。"),
    ("Размер значков в Dock", "Dock Icon Size", "Tamaño de iconos del Dock", "Symbolgröße im Dock", "Taille des icônes du Dock", "程序坞图标大小"),
    ("Увеличение значков при наведении", "Magnification on Hover", "Ampliación al pasar el cursor", "Vergrößerung beim Zeigen", "Agrandissement au survol", "鼠标悬停放大图标"),
    ("Положение на экране", "Position on Screen", "Posición en pantalla", "Position auf dem Bildschirm", "Position à l’écran", "屏幕上的位置"),
    ("Слева", "Left", "Izquierda", "Links", "À gauche", "左侧"),
    ("Снизу", "Bottom", "Abajo", "Unten", "En bas", "底部"),
    ("Внизу", "Bottom", "Abajo", "Unten", "En bas", "底部"),
    ("Справа", "Right", "Derecha", "Rechts", "À droite", "右侧"),
    ("Автоматически скрывать и показывать Dock", "Automatically Hide and Show the Dock", "Ocultar y mostrar el Dock automáticamente", "Dock automatisch ein- und ausblenden", "Masquer et afficher automatiquement le Dock", "自动隐藏和显示程序坞"),
    ("Активные углы", "Hot Corners", "Esquinas activas", "Aktive Ecken", "Coins actifs", "触发角"),
    ("Разрешение", "Resolution", "Resolución", "Auflösung", "Résolution", "分辨率"),
    ("Ночной режим", "Night Shift", "Night Shift", "Night Shift", "Night Shift", "夜览"),
    ("Тёплые цвета вечером для снижения нагрузки на глаза", "Warmer display colors at night to reduce eye strain", "Colores más cálidos de noche para descansar la vista", "Wärmere Farben am Abend zur Schonung der Augen", "Couleurs plus chaudes le soir pour reposer les yeux", "夜间使用较暖色调以减轻眼部疲劳"),

    // ── About This Computer & System Info ──
    ("О системе", "About This System", "Acerca del sistema", "Über dieses System", "À propos du système", "关于本系统"),
    ("Процессор", "Processor", "Procesador", "Prozessor", "Processeur", "处理器"),
    ("Память", "Memory", "Memoria", "Speicher", "Mémoire", "内存"),
    ("Видеокарта", "Graphics", "Gráficos", "Grafikkarte", "Graphisme", "显卡"),
    ("Диски", "Storage", "Almacenamiento", "Festplatten", "Stockage", "存储空间"),
    ("Память оболочки", "Shell Memory", "Memoria del entorno", "Shell-Speicher", "Mémoire de l’interface", "外壳内存"),
    ("Показывать при входе", "Show at Login", "Mostrar al iniciar sesión", "Beim Anmelden zeigen", "Afficher à l’ouverture", "登录时显示"),
    ("Сменить обои…", "Change Wallpaper…", "Cambiar fondo…", "Hintergrund ändern…", "Changer le fond…", "更换壁纸…"),
    ("Окон", "Windows", "Ventanas", "Fenster", "Fenêtres", "窗口数"),
    ("Композитор", "Compositor", "Compositor", "Compositor", "Compositeur", "合成器"),
    ("Рендер", "Renderer", "Renderizador", "Renderer", "Rendu", "渲染器"),
    ("Темп кадров", "Frame Pacing", "Ritmo de fotogramas", "Bildrate", "Cadence d’images", "帧率同步"),
    ("Дробный масштаб", "Fractional Scale", "Escala fraccionaria", "Skalierung", "Échelle fractionnaire", "分数缩放"),

    // ── Lock & Login Screen + Boot Splash ──
    ("Введите пароль", "Enter Password", "Introduce la contraseña", "Passwort eingeben", "Saisissez le mot de passe", "输入密码"),
    ("Неверный пароль", "Incorrect Password", "Contraseña incorrecta", "Falsches Passwort", "Mot de passe incorrect", "密码不正确"),
    ("Разблокировка…", "Unlocking…", "Desbloqueando…", "Entsperren…", "Déverrouillage…", "正在解锁…"),
    ("Заблокировать экран сейчас", "Lock Screen Now", "Bloquear pantalla ahora", "Bildschirm jetzt sperren", "Verrouiller l’écran maintenant", "立即锁定屏幕"),
    ("Заблокировать", "Lock", "Bloquear", "Sperren", "Verrouiller", "锁定"),
    ("Запуск графической среды…", "Starting desktop environment…", "Iniciando entorno gráfico…", "Grafische Umgebung wird gestartet…", "Démarrage de l’environnement graphique…", "正在启动图形桌面环境…"),
    ("Вход в систему Пёрышко", "Log In to Feather", "Iniciar sesión en Feather", "Bei Feather anmelden", "Connexion à Feather", "登录轻羽桌面"),
    ("Сеанс Пёрышко · Wayland", "Feather Session · Wayland", "Sesión Feather · Wayland", "Feather-Sitzung · Wayland", "Session Feather · Wayland", "轻羽会话 · Wayland"),
    ("Пёрышко DE", "Feather DE", "Feather DE", "Feather DE", "Feather DE", "轻羽桌面"),
    ("Сон", "Sleep", "Reposo", "Ruhezustand", "Suspendre", "睡眠"),
    ("Перезагрузить", "Restart", "Reiniciar", "Neustart", "Redémarrer", "重新启动"),
    ("Переход в режим сна…", "Entering sleep mode…", "Entrando en modo de reposo…", "Ruhezustand wird aktiviert…", "Mise en veille…", "正在进入睡眠模式…"),
    ("Перезагрузка…", "Restarting…", "Reiniciando…", "Neustart…", "Redémarrage…", "正在重新启动…"),
    ("Выключение…", "Shutting down…", "Apagando…", "Herunterfahren…", "Extinction…", "正在关机…"),
    ("Перезагрузка доступна в основном сеансе", "Restart is available in the primary session", "El reinicio está disponible en la sesión principal", "Neustart ist in der Hauptsitzung verfügbar", "Le redémarrage est disponible dans la session principale", "重新启动仅在主会话中可用"),
    ("Выключение доступно в основном сеансе", "Shut down is available in the primary session", "El apagado está disponible en la sesión principal", "Herunterfahren ist in der Hauptsitzung verfügbar", "L’extinction est disponible dans la session principale", "关机仅在主会话中可用"),

    // ── Files, Notes & File Picker ──
    ("Избранное", "Favorites", "Favoritos", "Favoriten", "Favoris", "个人收藏"),
    ("Домашняя", "Home", "Inicio", "Privat", "Départ", "个人主目录"),
    ("Рабочий стол", "Desktop", "Escritorio", "Schreibtisch", "Bureau", "桌面"),
    ("Документы", "Documents", "Documentos", "Dokumente", "Documents", "文稿"),
    ("Загрузки", "Downloads", "Descargas", "Downloads", "Téléchargements", "下载"),
    ("Изображения", "Pictures", "Imágenes", "Bilder", "Images", "图片"),
    ("Музыка", "Music", "Música", "Musik", "Musique", "音乐"),
    ("Видео", "Videos", "Vídeos", "Videos", "Vidéos", "影片"),
    ("Имя", "Name", "Nombre", "Name", "Nom", "名称"),
    ("Размер", "Size", "Tamaño", "Größe", "Taille", "大小"),
    ("Изменён", "Modified", "Modificado", "Geändert", "Modifié", "修改日期"),
    ("Тип", "Kind", "Clase", "Art", "Type", "种类"),
    ("Папка", "Folder", "Carpeta", "Ordner", "Dossier", "文件夹"),
    ("Документ", "Document", "Documento", "Dokument", "Document", "文稿"),
    ("Текст", "Text", "Texto", "Text", "Texte", "文本"),
    ("Исходный код", "Source Code", "Código fuente", "Quellcode", "Code source", "源代码"),
    ("Изображение", "Image", "Imagen", "Bild", "Image", "图像"),
    ("Аудио", "Audio", "Audio", "Audio", "Audio", "音频"),
    ("Архив", "Archive", "Archivo comprimido", "Archiv", "Archive", "归档"),
    ("Документ PDF", "PDF Document", "Documento PDF", "PDF-Dokument", "Document PDF", "PDF 文稿"),
    ("Программа", "Application", "Aplicación", "Programm", "Application", "应用程序"),
    ("Новая папка", "New Folder", "Nueva carpeta", "Neuer Ordner", "Nouveau dossier", "新建文件夹"),
    ("Создать папку", "New Folder", "Crear carpeta", "Neuer Ordner", "Nouveau dossier", "新建文件夹"),
    ("Переименовать", "Rename", "Renombrar", "Umbenennen", "Renommer", "重命名"),
    ("Переместить в Корзину", "Move to Trash", "Trasladar a la Papelera", "In den Papierkorb legen", "Placer dans la Corbeille", "移到废纸篓"),
    ("Очистить Корзину", "Empty Trash", "Vaciar Papelera", "Papierkorb entleeren", "Vider la Corbeille", "清倒废纸篓"),
    ("Свойства", "Get Info", "Obtener información", "Informationen", "Lire les informations", "显示简介"),
    ("Новая заметка", "New Note", "Nueva nota", "Neue Notiz", "Nouvelle note", "新建备忘录"),
    ("Все заметки", "All Notes", "Todas las notas", "Alle Notizen", "Toutes les notes", "所有备忘录"),
    ("Нет заметок", "No Notes", "No hay notas", "Keine Notizen", "Aucune note", "没有备忘录"),

    // ── Privacy & Screen Recording (Portal) ──
    ("Конфиденциальность", "Privacy", "Privacidad", "Datenschutz", "Confidentialité", "隐私"),
    ("Доступ программ к записи экрана и снимкам через системный портал.", "Application access to screen recording and screenshots through the system portal.", "Acceso de las aplicaciones a la grabación y capturas de pantalla mediante el portal del sistema.", "Programmzugriff auf Bildschirmaufnahme und Bildschirmfotos über das Systemportal.", "Accès des applications à l’enregistrement d’écran et aux captures via le portail système.", "通过系统门户管理应用程序对屏幕录制和截图的访问权限。"),
    ("Запись экрана и снимки (портал)", "Screen Recording & Screenshots (Portal)", "Grabación y capturas de pantalla (portal)", "Bildschirmaufnahme & Bildschirmfotos (Portal)", "Enregistrement d’écran et captures (portail)", "屏幕录制与截图（门户）"),
    ("Новые запросы доступа", "New Access Requests", "Nuevas solicitudes de acceso", "Neue Zugriffsanfragen", "Nouvelles demandes d’accès", "新访问请求"),
    ("ScreenCast, Screenshot и Access в xdg-desktop-portal", "ScreenCast, Screenshot, and Access in xdg-desktop-portal", "ScreenCast, Screenshot y Access en xdg-desktop-portal", "ScreenCast, Screenshot und Access in xdg-desktop-portal", "ScreenCast, Screenshot et Access dans xdg-desktop-portal", "xdg-desktop-portal 中的 ScreenCast、Screenshot 与 Access"),
    ("По списку", "Per App", "Por aplicación", "Nach Liste", "Par application", "按应用列表"),
    ("Всем", "Allow All", "Permitir todo", "Allen erlauben", "Tout autoriser", "全部允许"),
    ("Никому", "Block All", "Bloquear todo", "Niemandem", "Tout bloquer", "全部拒绝"),
    ("Спрашивать", "Ask", "Preguntar", "Fragen", "Demander", "询问"),
    ("Разрешать", "Allow", "Permitir", "Erlauben", "Autoriser", "允许"),
    ("Запрещать", "Deny", "Denegar", "Ablehnen", "Refuser", "拒绝"),
    ("Показывать курсор в записи", "Show Pointer in Recording", "Mostrar el puntero en la grabación", "Zeiger in der Aufnahme anzeigen", "Afficher le pointeur dans l’enregistrement", "在录制中显示指针"),
    ("Режим указателя по умолчанию для сеансов ScreenCast", "Default pointer visibility for ScreenCast sessions", "Visibilidad del puntero predeterminada para sesiones ScreenCast", "Standard-Zeigersichtbarkeit für ScreenCast-Sitzungen", "Visibilité du pointeur par défaut pour les sessions ScreenCast", "ScreenCast 会话的默认指针可见性"),
    ("Портал Пёрышка (org.freedesktop.impl.portal.desktop.feather) проверяет права программ перед захватом экрана и синхронизирует таблицу разрешений.", "The Feather portal (org.freedesktop.impl.portal.desktop.feather) verifies app permissions before screen capture and syncs the permission store.", "El portal de Feather (org.freedesktop.impl.portal.desktop.feather) verifica los permisos antes de capturar la pantalla y sincroniza la tabla de permisos.", "Das Feather-Portal (org.freedesktop.impl.portal.desktop.feather) prüft Programmrechte vor der Bildschirmaufnahme und synchronisiert die Berechtigungstabelle.", "Le portail Feather (org.freedesktop.impl.portal.desktop.feather) vérifie les droits des applications avant la capture d’écran et synchronise la table d’autorisations.", "Feather 门户（org.freedesktop.impl.portal.desktop.feather）会在捕获屏幕前校验应用权限并同步权限存储表。"),
    ("Доступ программ к экрану", "App Screen Access", "Acceso de aplicaciones a la pantalla", "Programmzugriff auf den Bildschirm", "Accès des applications à l’écran", "应用程序屏幕访问权限"),
    ("Список программ пуст", "No Applications Listed", "La lista de aplicaciones está vacía", "Keine Programme in der Liste", "Aucune application dans la liste", "应用列表为空"),
    ("Программы появятся здесь при обращении к порталу или после добавления вручную", "Apps appear here when they request portal access or when added manually", "Las aplicaciones aparecerán aquí al solicitar acceso al portal o al añadirlas manualmente", "Programme erscheinen hier bei einer Portalanfrage oder nach manuellem Hinzufügen", "Les applications apparaissent ici lors d’une demande au portail ou après ajout manuel", "应用在请求门户访问或手动添加后会显示在此处"),
    ("Разрешено", "Allowed", "Permitido", "Erlaubt", "Autorisé", "已允许"),
    ("Запрещено", "Blocked", "Bloqueado", "Blockiert", "Bloqué", "已阻止"),
    ("Добавить программу", "Add Application", "Añadir aplicación", "Programm hinzufügen", "Ajouter une application", "添加应用程序"),
    ("Выдать или ограничить доступ к записи экрана заранее", "Pre-authorize or restrict screen recording access for an application", "Autorizar o restringir de antemano el acceso a la grabación de pantalla", "Zugriff auf die Bildschirmaufnahme vorab erlauben oder einschränken", "Autoriser ou restreindre à l’avance l’accès à l’enregistrement d’écran", "预先授予或限制应用程序的屏幕录制访问权限"),
    ("Очистить", "Clear", "Borrar", "Leeren", "Effacer", "清空"),
    ("При политике «По списку» неизвестные программы добавляются при первом запросе; вы можете заранее разрешить или заблокировать любую программу.", "In “Per App” mode, unknown apps are recorded on first request; you can allow or block any app in advance.", "En el modo «Por aplicación», las aplicaciones desconocidas se registran en la primera solicitud; puedes permitir o bloquear cualquiera de antemano.", "Im Modus „Nach Liste“ werden unbekannte Programme bei der ersten Anfrage eingetragen; Sie können jedes Programm vorab erlauben oder blockieren.", "En mode « Par application », les applications inconnues sont ajoutées à la première demande ; vous pouvez autoriser ou bloquer toute application à l’avance.", "在“按应用列表”策略下，未知应用会在首次请求时加入列表；您可以预先允许或阻止任何应用。"),
    ("Разрешить доступ к записи экрана", "Allow Screen Recording Access", "Permitir acceso a la grabación de pantalla", "Zugriff auf Bildschirmaufnahme erlauben", "Autoriser l’accès à l’enregistrement d’écran", "允许访问屏幕录制"),

    // ── Preview («Просмотр») ──
    ("Просмотр", "Preview", "Vista Previa", "Vorschau", "Aperçu", "预览"),
    ("Изображения и документы PDF", "Images and PDF Documents", "Imágenes y documentos PDF", "Bilder und PDF-Dokumente", "Images et documents PDF", "图像与 PDF 文稿"),
    ("Открыть…", "Open…", "Abrir…", "Öffnen…", "Ouvrir…", "打开…"),
    ("Открыть файл…", "Open File…", "Abrir archivo…", "Datei öffnen…", "Ouvrir un fichier…", "打开文件…"),
    ("По окну", "Fit", "Ajustar", "Anpassen", "Adapter", "适合窗口"),
    ("Стр.", "Page", "Pág.", "Seite", "Page", "第"),
    ("из", "of", "de", "von", "sur", "/"),
    ("Сведения о документе", "Document Info", "Información del documento", "Dokumentinformationen", "Informations sur le document", "文稿信息"),
    ("Формат", "Format", "Formato", "Format", "Format", "格式"),
    ("Страниц", "Pages", "Páginas", "Seiten", "Pages", "页数"),
    ("Масштаб", "Zoom", "Zoom", "Zoom", "Zoom", "缩放"),
    ("Изображения PNG, JPEG, WebP, SVG и документы PDF", "PNG, JPEG, WebP, SVG Images and PDF Documents", "Imágenes PNG, JPEG, WebP, SVG y documentos PDF", "PNG-, JPEG-, WebP-, SVG-Bilder und PDF-Dokumente", "Images PNG, JPEG, WebP, SVG et documents PDF", "PNG、JPEG、WebP、SVG 图像与 PDF 文稿"),
    ("Перетащите файл сюда или нажмите «Открыть…»", "Drag a file here or click “Open…”", "Arrastra un archivo aquí o haz clic en «Abrir…»", "Datei hierher ziehen oder auf „Öffnen…“ klicken", "Glissez un fichier ici ou cliquez sur « Ouvrir… »", "将文件拖到此处或点按“打开…”"),
    ("Загрузка документа…", "Loading document…", "Cargando documento…", "Dokument wird geladen…", "Chargement du document…", "正在载入文稿…"),
    ("Не удалось открыть файл", "Could Not Open File", "No se pudo abrir el archivo", "Datei konnte nicht geöffnet werden", "Impossible d’ouvrir le fichier", "无法打开文件"),

    // ── TextEdit («Текстовый редактор») ──
    ("Текстовый редактор", "TextEdit", "Editor de Texto", "TextEdit", "TextEdit", "文本编辑"),
    ("Текстовые документы и исходный код", "Text Documents and Source Code", "Documentos de texto y código fuente", "Textdokumente und Quellcode", "Documents texte et code source", "文本文稿与源代码"),
    ("Без названия", "Untitled", "Sin título", "Ohne Titel", "Sans titre", "未命名"),
    ("Сохранить", "Save", "Guardar", "Sichern", "Enregistrer", "存储"),
    ("Сохранить документ", "Save Document", "Guardar documento", "Dokument sichern", "Enregistrer le document", "存储文稿"),
    ("Открыть текстовый документ", "Open Text Document", "Abrir documento de texto", "Textdokument öffnen", "Ouvrir un document texte", "打开文本文稿"),
    ("Текстовые файлы и код", "Text and Code Files", "Archivos de texto y código", "Text- und Codedateien", "Fichiers texte et code", "文本与代码文件"),
    ("Все файлы", "All Files", "Todos los archivos", "Alle Dateien", "Tous les fichiers", "所有文件"),
    ("Сохранено", "Saved", "Guardado", "Gesichert", "Enregistré", "已存储"),
    ("Изменено", "Edited", "Editado", "Bearbeitet", "Modifié", "已编辑"),
    ("Ошибка сохранения", "Save Failed", "Error al guardar", "Sichern fehlgeschlagen", "Échec de l’enregistrement", "存储失败"),
    ("Строк", "Lines", "Líneas", "Zeilen", "Lignes", "行数"),
    ("Слов", "Words", "Palabras", "Wörter", "Mots", "字数"),
    ("Симв.", "Chars", "Car.", "Zeichen", "Car.", "字符"),
    ("стб.", "Col", "col.", "Sp.", "col.", "列"),
    ("Выделено", "Selected", "Seleccionado", "Ausgewählt", "Sélectionné", "已选中"),
    ("Найти…", "Find…", "Buscar…", "Suchen…", "Rechercher…", "查找…"),
    ("Заменить на…", "Replace with…", "Reemplazar con…", "Ersetzen durch…", "Remplacer par…", "替换为…"),
    ("Заменить", "Replace", "Reemplazar", "Ersetzen", "Remplacer", "替换"),
    ("Все", "All", "Todo", "Alle", "Tout", "全部"),
    ("Заменено", "Replaced", "Reemplazado", "Ersetzt", "Remplacé", "已替换"),
    ("TOML / Конфиг", "TOML / Config", "TOML / Config.", "TOML / Konfig.", "TOML / Config.", "TOML / 配置"),
    ("Скрипт", "Script", "Script", "Skript", "Script", "脚本"),

    // ── Calculator («Калькулятор») ──
    ("Калькулятор", "Calculator", "Calculadora", "Rechner", "Calculette", "计算器"),
    ("Обычный, инженерный и программистский калькулятор", "Basic, Scientific, and Programmer Calculator", "Calculadora básica, científica y de programador", "Standard-, wissenschaftlicher und Programmierer-Rechner", "Calculette standard, scientifique et programmeur", "标准、科学型与编程器计算器"),
    ("Обычный", "Basic", "Básica", "Standard", "Standard", "标准"),
    ("Инженерный", "Scientific", "Científica", "Wissenschaftlich", "Scientifique", "科学型"),
    ("Программист", "Programmer", "Programador", "Programmierer", "Programmeur", "编程器"),
    ("Журнал", "History", "Historial", "Verlauf", "Historique", "历史记录"),
    ("Нет записей", "No History", "Sin historial", "Kein Verlauf", "Aucun historique", "无记录"),
    ("Не число", "Not a Number", "No es un número", "Keine Zahl", "Pas un nombre", "非数值"),
    ("Деление на 0", "Divide by Zero", "División por cero", "Division durch 0", "Division par zéro", "除以零"),
    ("Ошибка", "Error", "Error", "Fehler", "Erreur", "错误"),
    ("64 бит", "64-bit", "64 bits", "64-Bit", "64 bits", "64 位"),
    ("32 бит", "32-bit", "32 bits", "32-Bit", "32 bits", "32 位"),
    ("16 бит", "16-bit", "16 bits", "16-Bit", "16 bits", "16 位"),
    ("8 бит", "8-bit", "8 bits", "8-Bit", "8 bits", "8 位"),

    // ── First-Run Setup Assistant («Добро пожаловать в Пёрышко») ──
    ("Добро пожаловать в Пёрышко", "Welcome to Feather", "Bienvenido a Feather", "Willkommen bei Feather", "Bienvenue dans Feather", "欢迎使用轻羽"),
    ("Мастер приветствия…", "Welcome Assistant…", "Asistente de bienvenida…", "Willkommensassistent…", "Assistant de bienvenue…", "欢迎助理…"),
    ("Выберите язык интерфейса и региональные форматы.", "Choose your interface language and regional formats.", "Elige el idioma de la interfaz y los formatos regionales.", "Wählen Sie die Oberflächensprache und regionale Formate.", "Choisissez la langue de l’interface et les formats régionaux.", "选择界面语言与地区格式。"),
    ("Выберите тему оформления", "Choose Your Appearance", "Elige tu apariencia", "Erscheinungsbild wählen", "Choisissez votre apparence", "选择外观"),
    ("Настройте светлый или тёмный вид, цвет акцента и размер текста.", "Customize light or dark mode, accent color, and text size.", "Personaliza el modo claro u oscuro, el color de acento y el tamaño del texto.", "Passen Sie hellen oder dunklen Modus, Akzentfarbe und Textgröße an.", "Personnalisez le mode clair ou sombre, la couleur d’accentuation et la taille du texte.", "自定义浅色或深色外观、强调色和文字大小。"),
    ("Раскладка клавиатуры", "Keyboard Layouts", "Distribución del teclado", "Tastaturbelegung", "Disposition du clavier", "键盘布局"),
    ("Выберите используемые языки ввода и клавиши переключения.", "Select your input languages and layout switching shortcut.", "Selecciona tus idiomas de entrada y el atajo para cambiar de teclado.", "Wählen Sie Ihre Eingabesprachen und das Tastenkürzel zum Umschalten.", "Sélectionnez vos langues de saisie et le raccourci de changement de clavier.", "选择输入语言与切换布局的快捷键。"),
    ("Пёрышко готово к работе", "Feather Is Ready", "Feather está listo", "Feather ist bereit", "Feather est prêt", "轻羽已准备就绪"),
    ("Ваше окружение настроено — быстрый обзор главных возможностей.", "Your desktop is configured — here is a quick tour of key features.", "Tu entorno está configurado: un vistazo rápido a las funciones principales.", " Ihre Arbeitsumgebung ist eingerichtet — die wichtigsten Funktionen im Überblick.", "Votre environnement est configuré — aperçu rapide des fonctions clés.", "您的桌面环境已配置完成 — 快速了解核心功能。"),
    ("Шаг", "Step", "Paso", "Schritt", "Étape", "步骤"),
    ("Назад", "Back", "Atrás", "Zurück", "Retour", "返回"),
    ("Пропустить", "Skip", "Omitir", "Überspringen", "Ignorer", "跳过"),
    ("Продолжить", "Continue", "Continuar", "Fortfahren", "Continuer", "继续"),
    ("Начать работу", "Get Started", "Empezar", "Loslegen", "Commencer", "开始使用"),
    ("Регион и формат времени", "Region & Time Format", "Región y formato de hora", "Region & Zeitformat", "Région et format de l’heure", "地区与时间格式"),
    ("24‑часовой формат", "24-Hour Time", "Formato de 24 horas", "24-Stunden-Format", "Format 24 heures", "24 小时制"),
    ("Переключение языка", "Switch Input Source", "Cambiar idioma de entrada", "Eingabequelle wechseln", "Changer de source de saisie", "切换输入源"),
    ("Spotlight — мгновенный поиск программ и файлов", "Spotlight — instant search for apps and files", "Spotlight — búsqueda instantánea de apps y archivos", "Spotlight — Sofortsuche nach Programmen und Dateien", "Spotlight — recherche instantanée d’apps et de fichiers", "Spotlight — 即时搜索应用和文件"),
    ("Launchpad — сетка всех приложений", "Launchpad — full-screen application grid", "Launchpad — cuadrícula de todas las aplicaciones", "Launchpad — Vollbild-Programmübersicht", "Launchpad — grille de toutes les applications", "启动台 — 全屏应用网格"),
    ("Mission Control — обзор окон и рабочих столов", "Mission Control — overview of windows and desktops", "Mission Control — vista general de ventanas y escritorios", "Mission Control — Übersicht über Fenster und Schreibtische", "Mission Control — aperçu des fenêtres et bureaux", "调度中心 — 窗口与桌面概览"),
    ("Мгновенная блокировка экрана", "Lock screen immediately", "Bloquear pantalla al instante", "Bildschirm sofort sperren", "Verrouiller l’écran immédiatement", "立即锁定屏幕"),
];

fn lookup_exact(ru: &str, l: Lang) -> Option<&'static str> {
    for entry in TABLE {
        if entry.0 == ru {
            return Some(pick(entry, l));
        }
    }
    None
}

fn translate_dynamic(text: &str, l: Lang) -> Option<String> {
    if let Some(rest) = text.strip_prefix("Скрыть «").and_then(|s| s.strip_suffix('»')) {
        let name = tr(rest);
        return Some(match l {
            Lang::Ru => text.to_string(),
            Lang::En => format!("Hide “{name}”"),
            Lang::Es => format!("Ocultar «{name}»"),
            Lang::De => format!("„{name}“ ausblenden"),
            Lang::Fr => format!("Masquer « {name} »"),
            Lang::Zh => format!("隐藏“{name}”"),
        });
    }
    if let Some(rest) = text.strip_prefix("Завершить «").and_then(|s| s.strip_suffix('»')) {
        let name = tr(rest);
        return Some(match l {
            Lang::Ru => text.to_string(),
            Lang::En => format!("Quit “{name}”"),
            Lang::Es => format!("Salir de «{name}»"),
            Lang::De => format!("„{name}“ beenden"),
            Lang::Fr => format!("Quitter « {name} »"),
            Lang::Zh => format!("退出“{name}”"),
        });
    }
    if let Some(title) = text.strip_suffix(" (свёрнуто)") {
        return Some(match l {
            Lang::Ru => text.to_string(),
            Lang::En => format!("{title} (minimized)"),
            Lang::Es => format!("{title} (minimizado)"),
            Lang::De => format!("{title} (minimiert)"),
            Lang::Fr => format!("{title} (réduit)"),
            Lang::Zh => format!("{title}（已最小化）"),
        });
    }
    if let Some(num) = text.strip_prefix("Рабочий стол ") {
        return Some(match l {
            Lang::Ru => text.to_string(),
            Lang::En => format!("Desktop {num}"),
            Lang::Es => format!("Escritorio {num}"),
            Lang::De => format!("Schreibtisch {num}"),
            Lang::Fr => format!("Bureau {num}"),
            Lang::Zh => format!("桌面 {num}"),
        });
    }
    if let Some(prefix) = text.strip_suffix(" Гц") {
        let hz = match l {
            Lang::Ru => "Гц",
            Lang::Zh => "Hz",
            _ => "Hz",
        };
        return Some(format!("{prefix} {hz}"));
    }
    if let Some(prefix) = text.strip_suffix(" МБ") {
        return Some(format!("{prefix} MB"));
    }
    if let Some(prefix) = text.strip_suffix(" ГБ") {
        return Some(format!("{prefix} GB"));
    }
    if let Some(prefix) = text.strip_suffix(" ТБ") {
        return Some(format!("{prefix} TB"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_and_region_roundtrip_and_translations() {
        for lang in Lang::ALL {
            assert_eq!(Lang::from_u8(lang as u8), lang);
            assert_eq!(Lang::parse(lang.id()), Some(lang));
        }
        for region in Region::ALL {
            assert_eq!(Region::from_u8(region as u8), region);
            assert_eq!(Region::parse(region.id()), Some(region));
            assert_eq!(Region::parse(region.locale_code()), Some(region));
        }
        assert_eq!(lookup_exact("Системные настройки", Lang::En), Some("System Settings"));
        assert_eq!(lookup_exact("Системные настройки", Lang::Es), Some("Ajustes del Sistema"));
        assert_eq!(lookup_exact("Системные настройки", Lang::De), Some("Systemeinstellungen"));
        assert_eq!(lookup_exact("Системные настройки", Lang::Fr), Some("Réglages Système"));
        assert_eq!(lookup_exact("Системные настройки", Lang::Zh), Some("系统设置"));
        assert_eq!(
            translate_dynamic("Скрыть «Firefox»", Lang::En).as_deref(),
            Some("Hide “Firefox”")
        );
    }
}
