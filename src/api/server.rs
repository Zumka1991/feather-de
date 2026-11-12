//! `org.feather.Shell1` на сессионной шине. Каждый метод превращается в `Call`, уходит в цикл
//! событий через ограниченный calloop-канал и ждёт ответа на одноразовом канале. Цикл никогда
//! не ждёт этот поток: запросы отправляются `try_send`, события — тоже.
use super::{automation::Input, screenshot_target, space_index, Call, Event, Reply, Request, Responder};
use feather_api::{AppInfo, AudioInfo, DisplayInfo, Error, MediaInfo, ShelfInfo, SpaceInfo, WindowInfo};
use smithay::reexports::calloop::channel;
use zbus::{object_server::SignalEmitter, Connection};

pub struct Server {
    pub requests: channel::SyncSender<Request>,
    automation_enabled: bool,
}

fn unexpected() -> Error {
    Error::Failed("Неожиданный ответ композитора".into())
}

/// Ответ нужного вида или ошибка
macro_rules! want {
    ($reply:expr, $pat:pat => $value:expr) => {
        match $reply? {
            $pat => Ok($value),
            _ => Err(unexpected()),
        }
    };
}

impl Server {
    async fn call(&self, call: Call) -> Result<Reply, Error> {
        let (tx, rx) = async_channel::bounded(1);
        self.requests.try_send(Request { call, reply: Responder(tx) }).map_err(|e| {
            Error::Failed(match e {
                std::sync::mpsc::TrySendError::Full(_) => "Пёрышко занято, повторите запрос".into(),
                std::sync::mpsc::TrySendError::Disconnected(_) => "Пёрышко завершает работу".into(),
            })
        })?;
        // A dropped responder (the request could not finish) is an error, never a hang
        rx.recv().await.map_err(|_| Error::Failed("Пёрышко не выполнило запрос".into()))?
    }

    async fn done(&self, call: Call) -> Result<(), Error> {
        want!(self.call(call).await, Reply::Done => ())
    }

    async fn percent(&self, call: Call) -> Result<u32, Error> {
        want!(self.call(call).await, Reply::Percent(p) => p)
    }
}

#[zbus::interface(name = "org.feather.Shell1")]
impl Server {
    // Окна
    async fn list_windows(&self) -> Result<Vec<WindowInfo>, Error> {
        want!(self.call(Call::ListWindows).await, Reply::Windows(v) => v)
    }
    async fn focus_window(&self, id: u64) -> Result<(), Error> {
        self.done(Call::FocusWindow(id)).await
    }
    async fn close_window(&self, id: u64) -> Result<(), Error> {
        self.done(Call::CloseWindow(id)).await
    }
    async fn minimize_window(&self, id: u64) -> Result<(), Error> {
        self.done(Call::MinimizeWindow(id)).await
    }
    async fn maximize_window(&self, id: u64, maximized: bool) -> Result<(), Error> {
        self.done(Call::MaximizeWindow(id, maximized)).await
    }
    async fn fullscreen_window(&self, id: u64, fullscreen: bool) -> Result<(), Error> {
        self.done(Call::FullscreenWindow(id, fullscreen)).await
    }
    async fn move_window_to_space(&self, id: u64, space: u32) -> Result<(), Error> {
        self.done(Call::MoveWindowToSpace(id, space_index(space)?)).await
    }

    // Рабочие столы
    async fn list_spaces(&self) -> Result<Vec<SpaceInfo>, Error> {
        want!(self.call(Call::ListSpaces).await, Reply::Spaces(v) => v)
    }
    async fn switch_space(&self, number: u32) -> Result<(), Error> {
        self.done(Call::SwitchSpace(space_index(number)?)).await
    }
    async fn get_space_mode(&self, number: u32) -> Result<String, Error> {
        want!(self.call(Call::GetSpaceMode(super::space_target(number)?)).await, Reply::SpaceMode(m) => m)
    }
    async fn set_space_mode(&self, number: u32, mode: String) -> Result<(), Error> {
        self.done(Call::SetSpaceMode(super::space_target(number)?, super::space_mode_target(&mode)?)).await
    }
    async fn show_mission_control(&self, show: bool) -> Result<(), Error> {
        self.done(Call::MissionControl(show)).await
    }
    async fn show_launchpad(&self, show: bool) -> Result<(), Error> {
        self.done(Call::Launchpad(show)).await
    }

    // Полка перетаскивания
    async fn get_drop_shelf(&self) -> Result<ShelfInfo, Error> {
        want!(self.call(Call::GetDropShelf).await, Reply::DropShelf(s) => s)
    }
    async fn show_drop_shelf(&self, show: bool) -> Result<(), Error> {
        self.done(Call::ShowDropShelf(show)).await
    }
    async fn clear_drop_shelf(&self) -> Result<(), Error> {
        self.done(Call::ClearDropShelf).await
    }
    async fn add_to_drop_shelf(&self, paths: Vec<String>) -> Result<u32, Error> {
        if paths.is_empty() || paths.len() > 64 {
            return Err(Error::InvalidArgs("Укажите от 1 до 64 путей".into()));
        }
        self.percent(Call::AddToDropShelf(paths)).await
    }

    // Приложения
    async fn list_apps(&self) -> Result<Vec<AppInfo>, Error> {
        want!(self.call(Call::ListApps).await, Reply::Apps(v) => v)
    }
    async fn launch_app(&self, app_id: String) -> Result<(), Error> {
        self.done(Call::LaunchApp(super::app_query(app_id)?)).await
    }
    async fn activate_app(&self, app_id: String) -> Result<(), Error> {
        self.done(Call::ActivateApp(super::app_query(app_id)?)).await
    }

    // Звук
    async fn get_audio(&self) -> Result<AudioInfo, Error> {
        want!(self.call(Call::GetAudio).await, Reply::Audio(a) => a)
    }
    async fn set_volume(&self, percent: u32) -> Result<u32, Error> {
        self.percent(Call::SetVolume(percent.min(100))).await
    }
    async fn change_volume(&self, delta: i32) -> Result<u32, Error> {
        self.percent(Call::ChangeVolume(delta)).await
    }
    async fn set_mute(&self, muted: bool) -> Result<(), Error> {
        self.done(Call::SetMute(muted)).await
    }
    async fn set_mic_mute(&self, muted: bool) -> Result<(), Error> {
        self.done(Call::SetMicMute(muted)).await
    }

    // Экран и режимы
    async fn get_display(&self) -> Result<DisplayInfo, Error> {
        want!(self.call(Call::GetDisplay).await, Reply::Display(d) => d)
    }
    async fn set_brightness(&self, percent: u32) -> Result<u32, Error> {
        self.percent(Call::SetBrightness(percent.min(100))).await
    }
    async fn change_brightness(&self, delta: i32) -> Result<u32, Error> {
        self.percent(Call::ChangeBrightness(delta)).await
    }
    async fn set_night_shift(&self, on: bool) -> Result<(), Error> {
        self.done(Call::SetNightShift(on)).await
    }
    async fn set_dark_mode(&self, on: bool) -> Result<(), Error> {
        self.done(Call::SetDarkMode(on)).await
    }
    async fn set_do_not_disturb(&self, on: bool) -> Result<(), Error> {
        self.done(Call::SetDoNotDisturb(on)).await
    }
    async fn get_dock_edge(&self) -> Result<String, Error> {
        want!(self.call(Call::GetDockEdge).await, Reply::DockEdge(edge) => edge)
    }
    async fn set_dock_edge(&self, edge: String) -> Result<(), Error> {
        let edge = crate::theme::DockEdge::parse(&edge)
            .ok_or_else(|| Error::InvalidArgs("Положение Dock: left, bottom или right".into()))?;
        self.done(Call::SetDockEdge(edge)).await
    }

    // Медиа
    async fn get_media(&self) -> Result<MediaInfo, Error> {
        want!(self.call(Call::GetMedia).await, Reply::Media(m) => m)
    }
    async fn media_play_pause(&self) -> Result<(), Error> {
        self.done(Call::Media(crate::mpris::Command::PlayPause)).await
    }
    async fn media_next(&self) -> Result<(), Error> {
        self.done(Call::Media(crate::mpris::Command::Next)).await
    }
    async fn media_previous(&self) -> Result<(), Error> {
        self.done(Call::Media(crate::mpris::Command::Previous)).await
    }

    // Система
    async fn screenshot(&self, target: String) -> Result<String, Error> {
        want!(self.call(Call::Screenshot(screenshot_target(&target)?)).await, Reply::Path(p) => p)
    }
    #[zbus(out_args("token", "width", "height", "png"))]
    async fn observe_window(&self, id: u64) -> Result<(u64, u32, u32, Vec<u8>), Error> {
        want!(self.call(Call::ObserveWindow(id)).await, Reply::WindowImage(t, w, h, png) => (t, w, h, png))
    }
    async fn window_click(&self, token: u64, x: u32, y: u32, button: u32, count: u32) -> Result<(), Error> {
        if !(1..=3).contains(&button) || !(1..=2).contains(&count) {
            return Err(Error::InvalidArgs("Кнопка: 1–3, число нажатий: 1–2".into()));
        }
        self.done(Call::WindowInput(token, Input::Click { x, y, button, count })).await
    }
    async fn window_scroll(&self, token: u64, x: u32, y: u32, steps: i32) -> Result<(), Error> {
        if steps == 0 || !(-12..=12).contains(&steps) {
            return Err(Error::InvalidArgs("Прокрутка: от −12 до 12 шагов".into()));
        }
        self.done(Call::WindowInput(token, Input::Scroll { x, y, steps })).await
    }
    async fn window_key(&self, token: u64, keys: Vec<u32>) -> Result<(), Error> {
        super::automation::validate_keys(&keys)?;
        self.done(Call::WindowInput(token, Input::Key(keys))).await
    }
    async fn window_paste(&self, token: u64, text: String, keys: Vec<u32>) -> Result<(), Error> {
        if text.is_empty() || text.len() > 65_536 {
            return Err(Error::InvalidArgs("Вставляемый текст: от 1 до 65536 байт".into()));
        }
        super::automation::validate_keys(&keys)?;
        self.done(Call::WindowInput(token, Input::Paste(text, keys))).await
    }
    async fn lock_screen(&self) -> Result<(), Error> {
        self.done(Call::LockScreen).await
    }
    async fn suspend(&self) -> Result<(), Error> {
        self.done(Call::Suspend).await
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        feather_api::VERSION
    }
    #[zbus(property)]
    fn compositor_version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
    async fn stop_larvis_desktop(&self) -> Result<(), Error> {
        self.done(Call::StopLarvisDesktop).await
    }
    /// (запущен, показан)
    async fn get_larvis_desktop(&self) -> Result<(bool, bool), Error> {
        want!(self.call(Call::GetLarvisDesktop).await, Reply::LarvisDesktop(running, shown) => (running, shown))
    }
    async fn show_larvis_desktop(&self, show: bool) -> Result<(), Error> {
        self.done(Call::ShowLarvisDesktop(show)).await
    }

    #[zbus(property)]
    fn larvis_desktop_supported(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn automation_enabled(&self) -> bool {
        self.automation_enabled
    }

    #[zbus(signal)]
    async fn window_opened(emitter: &SignalEmitter<'_>, id: u64, app_id: &str, title: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn window_closed(emitter: &SignalEmitter<'_>, id: u64) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn focus_changed(emitter: &SignalEmitter<'_>, id: u64) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn space_changed(emitter: &SignalEmitter<'_>, number: u32, count: u32, larvis: bool) -> zbus::Result<()>;
}

/// `org.feather.ScreenCast1`: потоки демонстрации экрана для портала Пёрышка
/// (`org.freedesktop.impl.portal.ScreenCast`). Разрешение спрашивает портал; здесь — только
/// создание и остановка потока PipeWire.
pub struct Cast {
    requests: channel::SyncSender<Request>,
}

impl Cast {
    async fn call(&self, call: Call) -> Result<Reply, Error> {
        Server { requests: self.requests.clone(), automation_enabled: false }.call(call).await
    }
}

#[zbus::interface(name = "org.feather.ScreenCast1")]
impl Cast {
    /// `cursor_mode`: 1 — без курсора, 2 — курсор в кадре. Пустой `monitor` — монитор под
    /// курсором. Ответ: id потока, узел PipeWire и логический прямоугольник монитора.
    async fn start(&self, cursor_mode: u32, monitor: String) -> Result<(u64, u32, i32, i32, i32, i32), Error> {
        if monitor.len() > 64 {
            return Err(Error::InvalidArgs("Слишком длинное имя монитора".into()));
        }
        want!(self.call(Call::CastStart(cursor_mode != 1, monitor)).await,
            Reply::Cast { stream, node, rect } => (stream, node, rect.0, rect.1, rect.2, rect.3))
    }

    async fn stop(&self, stream: u64) -> Result<(), Error> {
        want!(self.call(Call::CastStop(stream)).await, Reply::Done => ())
    }
}

pub async fn run(requests: channel::SyncSender<Request>, events: async_channel::Receiver<Event>, name: String, replace: bool, automation_enabled: bool) -> zbus::Result<()> {
    let conn = zbus::connection::Builder::session()?.build().await?;
    serve(&conn, requests, events, &name, replace, automation_enabled).await
}

/// Our own session takes the name over (a stale nested run may hold it); a nested run never
/// takes it from the real session. Either one lets a later owner replace it.
async fn serve(conn: &Connection, requests: channel::SyncSender<Request>, events: async_channel::Receiver<Event>, name: &str, replace: bool, automation_enabled: bool) -> zbus::Result<()> {
    use zbus::fdo::{RequestNameFlags as F, RequestNameReply as R};
    conn.object_server().at(feather_api::PATH, Server { requests: requests.clone(), automation_enabled }).await?;
    conn.object_server().at(feather_api::CAST_PATH, Cast { requests }).await?;
    let mut flags = F::AllowReplacement | F::DoNotQueue;
    if replace {
        flags |= F::ReplaceExisting;
    }
    let name = zbus::names::WellKnownName::try_from(name)?;
    if !matches!(conn.request_name_with_flags(name.clone(), flags).await, Ok(R::PrimaryOwner | R::AlreadyOwner)) {
        tracing::info!("API: имя {name} занято другим Пёрышком");
        return Ok(());
    }
    tracing::info!("API: {name} на сессионной шине");
    let emitter = SignalEmitter::new(conn, feather_api::PATH)?;
    while let Ok(event) = events.recv().await {
        let _ = match event {
            Event::Opened { id, app_id, title } => Server::window_opened(&emitter, id, &app_id, &title).await,
            Event::Closed(id) => Server::window_closed(&emitter, id).await,
            Event::Focus(id) => Server::focus_changed(&emitter, id).await,
            Event::Space { number, count, larvis } => Server::space_changed(&emitter, number, count, larvis).await,
        };
    }
    Ok(())
}
