//! API рабочего стола Пёрышка: интерфейс `org.feather.Shell1` на сессионной шине D-Bus.
//!
//! Сервер живёт в композиторе (`src/api/`), здесь — то, что нужно обеим сторонам: имена,
//! типы ответов, ошибки и (с feature `client`) блокирующий/асинхронный прокси для внешних
//! программ и `featherctl`. Полное описание методов — `docs/API.md`.
//!
//! Нумерация рабочих столов в API начинается с 1. Окно с id 0 — активное (в фокусе).
use serde::{Deserialize, Serialize};
use zvariant::Type;

/// Имя на сессионной шине
pub const BUS_NAME: &str = "org.feather.Shell1";
pub const PATH: &str = "/org/feather/Shell1";
pub const INTERFACE: &str = "org.feather.Shell1";
/// Версия интерфейса (свойство `Version`): растёт при несовместимых изменениях
pub const VERSION: u32 = 1;
/// Другое имя на шине — для тестового экземпляра рядом с настоящим Пёрышком
pub const NAME_ENV: &str = "FEATHER_API_NAME";

/// Методы, которые могут потерять несохранённые данные пользователя: клиент (Ларвис)
/// спрашивает подтверждение до вызова
pub const DANGEROUS: &[&str] = &["CloseWindow", "Suspend"];

/// Имя сервиса: `FEATHER_API_NAME`, если это допустимое имя D-Bus, иначе `org.feather.Shell1`
pub fn bus_name() -> String {
    std::env::var(NAME_ENV)
        .ok()
        .filter(|n| zbus::names::WellKnownName::try_from(n.as_str()).is_ok())
        .unwrap_or_else(|| BUS_NAME.to_string())
}

/// Окно верхнего уровня (`ListWindows`), D-Bus `(tsssubbbb)`
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct WindowInfo {
    pub id: u64,
    /// Wayland app_id (или класс X11)
    pub app_id: String,
    /// Имя приложения из `.desktop`; пусто, если программа неизвестна
    pub app_name: String,
    pub title: String,
    /// Рабочий стол, с 1
    pub space: u32,
    pub focused: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub fullscreen: bool,
}

/// Рабочий стол (`ListSpaces`), D-Bus `(ubu)`
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SpaceInfo {
    /// С 1
    pub number: u32,
    pub current: bool,
    /// Открытых окон на столе, свёрнутые тоже считаются
    pub windows: u32,
}

/// Приложение (`ListApps`), D-Bus `(sssb)`
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct AppInfo {
    /// Для `LaunchApp`/`ActivateApp`: имя `.desktop` без расширения или `builtin.*`
    pub id: String,
    pub name: String,
    pub description: String,
    /// Есть открытые окна
    pub running: bool,
}

/// Звук (`GetAudio`), D-Bus `(bsubbsub)`. Громкость — проценты (0–100, у PipeWire бывает больше)
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct AudioInfo {
    /// Есть устройство вывода по умолчанию
    pub available: bool,
    pub output: String,
    pub volume: u32,
    pub muted: bool,
    pub mic_available: bool,
    pub input: String,
    pub mic_volume: u32,
    pub mic_muted: bool,
}

/// Экран и режимы (`GetDisplay`), D-Bus `(ibbb)`
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DisplayInfo {
    /// Яркость в процентах; -1 — экран без управления яркостью (или вложенный запуск)
    pub brightness: i32,
    pub night_shift: bool,
    pub dark_mode: bool,
    pub do_not_disturb: bool,
}

/// Текущий плеер MPRIS2 (`GetMedia`), D-Bus `(bsssb)`
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MediaInfo {
    pub available: bool,
    pub player: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
}

/// Ошибки методов: `org.feather.Shell1.Error.<Вариант>` с описанием по-русски
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.feather.Shell1.Error")]
pub enum Error {
    #[zbus(error)]
    ZBus(zbus::Error),
    /// Нет такого окна, приложения или стола
    NotFound(String),
    InvalidArgs(String),
    /// Служба (звук, яркость, плеер) недоступна в этом сеансе
    Unavailable(String),
    Failed(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(feature = "client")]
mod client {
    use super::*;

    /// Клиент `org.feather.Shell1`. Имена методов D-Bus — те же в PascalCase.
    #[zbus::proxy(interface = "org.feather.Shell1", default_service = "org.feather.Shell1", default_path = "/org/feather/Shell1")]
    pub trait Shell1 {
        fn list_windows(&self) -> Result<Vec<WindowInfo>>;
        fn focus_window(&self, id: u64) -> Result<()>;
        /// Опасно: программа может закрыться без сохранения
        fn close_window(&self, id: u64) -> Result<()>;
        fn minimize_window(&self, id: u64) -> Result<()>;
        fn maximize_window(&self, id: u64, maximized: bool) -> Result<()>;
        fn fullscreen_window(&self, id: u64, fullscreen: bool) -> Result<()>;
        fn move_window_to_space(&self, id: u64, space: u32) -> Result<()>;

        fn list_spaces(&self) -> Result<Vec<SpaceInfo>>;
        fn switch_space(&self, number: u32) -> Result<()>;
        fn show_mission_control(&self, show: bool) -> Result<()>;
        fn show_launchpad(&self, show: bool) -> Result<()>;

        fn list_apps(&self) -> Result<Vec<AppInfo>>;
        fn launch_app(&self, app_id: &str) -> Result<()>;
        fn activate_app(&self, app_id: &str) -> Result<()>;

        fn get_audio(&self) -> Result<AudioInfo>;
        fn set_volume(&self, percent: u32) -> Result<u32>;
        fn change_volume(&self, delta: i32) -> Result<u32>;
        fn set_mute(&self, muted: bool) -> Result<()>;
        fn set_mic_mute(&self, muted: bool) -> Result<()>;

        fn get_display(&self) -> Result<DisplayInfo>;
        fn set_brightness(&self, percent: u32) -> Result<u32>;
        fn change_brightness(&self, delta: i32) -> Result<u32>;
        fn set_night_shift(&self, on: bool) -> Result<()>;
        fn set_dark_mode(&self, on: bool) -> Result<()>;
        fn set_do_not_disturb(&self, on: bool) -> Result<()>;
        fn get_dock_edge(&self) -> Result<String>;
        fn set_dock_edge(&self, edge: &str) -> Result<()>;

        fn get_media(&self) -> Result<MediaInfo>;
        fn media_play_pause(&self) -> Result<()>;
        fn media_next(&self) -> Result<()>;
        fn media_previous(&self) -> Result<()>;

        /// `screen` или `window`; возвращает путь к сохранённому файлу
        fn screenshot(&self, target: &str) -> Result<String>;
        fn lock_screen(&self) -> Result<()>;
        /// Опасно: сон
        fn suspend(&self) -> Result<()>;

        #[zbus(property)]
        fn version(&self) -> zbus::Result<u32>;
        #[zbus(property)]
        fn compositor_version(&self) -> zbus::Result<String>;

        #[zbus(signal)]
        fn window_opened(&self, id: u64, app_id: String, title: String) -> zbus::Result<()>;
        #[zbus(signal)]
        fn window_closed(&self, id: u64) -> zbus::Result<()>;
        /// 0 — фокуса нет ни у одного окна
        #[zbus(signal)]
        fn focus_changed(&self, id: u64) -> zbus::Result<()>;
    }

    /// Подключение к сессионной шине и прокси на `bus_name()`
    pub fn connect() -> Result<Shell1ProxyBlocking<'static>> {
        let conn = zbus::blocking::Connection::session()?;
        Ok(Shell1ProxyBlocking::builder(&conn).destination(bus_name())?.build()?)
    }
}

#[cfg(feature = "client")]
pub use client::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_are_stable() {
        assert_eq!(WindowInfo::SIGNATURE.to_string(), "(tsssubbbb)");
        assert_eq!(SpaceInfo::SIGNATURE.to_string(), "(ubu)");
        assert_eq!(AppInfo::SIGNATURE.to_string(), "(sssb)");
        assert_eq!(AudioInfo::SIGNATURE.to_string(), "(bsubbsub)");
        assert_eq!(DisplayInfo::SIGNATURE.to_string(), "(ibbb)");
        assert_eq!(MediaInfo::SIGNATURE.to_string(), "(bsssb)");
    }

    #[test]
    fn errors_carry_their_dbus_names() {
        use zbus::DBusError;
        assert_eq!(Error::NotFound("x".into()).name().as_str(), "org.feather.Shell1.Error.NotFound");
        assert_eq!(Error::Unavailable("x".into()).description(), Some("x"));
    }

    #[test]
    fn bus_name_override_must_be_valid() {
        // One test touches the variable: no other test reads it concurrently
        std::env::set_var(NAME_ENV, "org.feather.Shell1.Test");
        assert_eq!(bus_name(), "org.feather.Shell1.Test");
        std::env::set_var(NAME_ENV, "не имя");
        assert_eq!(bus_name(), BUS_NAME);
        std::env::remove_var(NAME_ENV);
        assert_eq!(bus_name(), BUS_NAME);
    }
}
