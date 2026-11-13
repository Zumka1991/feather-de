//! `org.feather.Shell1` на сессионной шине. Каждый метод превращается в `Call`, уходит в цикл
//! событий через ограниченный calloop-канал и ждёт ответа на одноразовом канале. Цикл никогда
//! не ждёт этот поток: запросы отправляются `try_send`, события — тоже.
use super::{automation::Input, screenshot_target, space_index, Call, Event, Reply, Request, Responder};
use feather_api::{AppInfo, AudioInfo, AudioStreamInfo, DisplayInfo, Error, MediaInfo, ShelfInfo, SpaceInfo, WindowInfo};
use smithay::reexports::calloop::channel;
use std::{collections::HashMap, sync::Mutex};
use zbus::{message::Header, object_server::SignalEmitter, Connection};

pub struct Server {
    pub requests: channel::SyncSender<Request>,
    automation_enabled: bool,
    caller_cache: Mutex<HashMap<String, bool>>,
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

async fn send_request(requests: &channel::SyncSender<Request>, call: Call) -> Result<Reply, Error> {
    let (tx, rx) = async_channel::bounded(1);
    requests.try_send(Request { call, reply: Responder(tx) }).map_err(|e| {
        Error::Failed(match e {
            std::sync::mpsc::TrySendError::Full(_) => "Пёрышко занято, повторите запрос".into(),
            std::sync::mpsc::TrySendError::Disconnected(_) => "Пёрышко завершает работу".into(),
        })
    })?;
    // A dropped responder (the request could not finish) is an error, never a hang
    rx.recv().await.map_err(|_| Error::Failed("Пёрышко не выполнило запрос".into()))?
}

impl Server {
    async fn call(&self, header: &Header<'_>, conn: &Connection, call: Call) -> Result<Reply, Error> {
        crate::dbus_auth::verify_shell_caller(conn, header, &self.caller_cache).await?;
        send_request(&self.requests, call).await
    }

    async fn done(&self, header: &Header<'_>, conn: &Connection, call: Call) -> Result<(), Error> {
        want!(self.call(header, conn, call).await, Reply::Done => ())
    }

    async fn percent(&self, header: &Header<'_>, conn: &Connection, call: Call) -> Result<u32, Error> {
        want!(self.call(header, conn, call).await, Reply::Percent(p) => p)
    }
}

#[zbus::interface(name = "org.feather.Shell1")]
impl Server {
    // Окна
    async fn list_windows(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<Vec<WindowInfo>, Error> {
        want!(self.call(&h, c, Call::ListWindows).await, Reply::Windows(v) => v)
    }
    async fn focus_window(&self, id: u64, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::FocusWindow(id)).await
    }
    async fn close_window(&self, id: u64, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::CloseWindow(id)).await
    }
    async fn minimize_window(&self, id: u64, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::MinimizeWindow(id)).await
    }
    async fn maximize_window(&self, id: u64, maximized: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::MaximizeWindow(id, maximized)).await
    }
    async fn fullscreen_window(&self, id: u64, fullscreen: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::FullscreenWindow(id, fullscreen)).await
    }
    async fn move_window_to_space(&self, id: u64, space: u32, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::MoveWindowToSpace(id, space_index(space)?)).await
    }
    async fn get_window_pip(&self, id: u64, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(bool, bool), Error> {
        want!(self.call(&h, c, Call::GetWindowPip(id)).await, Reply::WindowPip(pip, ghost) => (pip, ghost))
    }
    async fn set_window_pip(&self, id: u64, pip: bool, ghost: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::SetWindowPip(id, pip, ghost)).await
    }

    // Рабочие столы
    async fn list_spaces(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<Vec<SpaceInfo>, Error> {
        want!(self.call(&h, c, Call::ListSpaces).await, Reply::Spaces(v) => v)
    }
    async fn switch_space(&self, number: u32, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::SwitchSpace(space_index(number)?)).await
    }
    async fn get_space_mode(&self, number: u32, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<String, Error> {
        want!(self.call(&h, c, Call::GetSpaceMode(super::space_target(number)?)).await, Reply::SpaceMode(m) => m)
    }
    async fn set_space_mode(&self, number: u32, mode: String, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::SetSpaceMode(super::space_target(number)?, super::space_mode_target(&mode)?)).await
    }
    async fn show_mission_control(&self, show: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::MissionControl(show)).await
    }
    async fn show_launchpad(&self, show: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::Launchpad(show)).await
    }
    async fn get_menu_hud(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<bool, Error> {
        want!(self.call(&h, c, Call::GetMenuHud).await, Reply::Bool(b) => b)
    }
    async fn show_menu_hud(&self, show: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::ShowMenuHud(show)).await
    }

    // Полка перетаскивания
    async fn get_drop_shelf(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<ShelfInfo, Error> {
        want!(self.call(&h, c, Call::GetDropShelf).await, Reply::DropShelf(s) => s)
    }
    async fn show_drop_shelf(&self, show: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::ShowDropShelf(show)).await
    }
    async fn clear_drop_shelf(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::ClearDropShelf).await
    }
    async fn add_to_drop_shelf(&self, paths: Vec<String>, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<u32, Error> {
        if paths.is_empty() || paths.len() > 64 {
            return Err(Error::InvalidArgs("Укажите от 1 до 64 путей".into()));
        }
        self.percent(&h, c, Call::AddToDropShelf(paths)).await
    }

    // Приложения
    async fn list_apps(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<Vec<AppInfo>, Error> {
        want!(self.call(&h, c, Call::ListApps).await, Reply::Apps(v) => v)
    }
    async fn launch_app(&self, app_id: String, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::LaunchApp(super::app_query(app_id)?)).await
    }
    async fn activate_app(&self, app_id: String, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::ActivateApp(super::app_query(app_id)?)).await
    }

    // Звук
    async fn get_audio(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<AudioInfo, Error> {
        want!(self.call(&h, c, Call::GetAudio).await, Reply::Audio(a) => a)
    }
    async fn set_volume(&self, percent: u32, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<u32, Error> {
        self.percent(&h, c, Call::SetVolume(percent.min(100))).await
    }
    async fn change_volume(&self, delta: i32, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<u32, Error> {
        self.percent(&h, c, Call::ChangeVolume(delta)).await
    }
    async fn set_mute(&self, muted: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::SetMute(muted)).await
    }
    async fn set_mic_mute(&self, muted: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::SetMicMute(muted)).await
    }
    async fn list_audio_streams(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<Vec<AudioStreamInfo>, Error> {
        want!(self.call(&h, c, Call::ListAudioStreams).await, Reply::AudioStreams(v) => v)
    }
    async fn set_stream_volume(&self, index: u32, percent: u32, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<u32, Error> {
        self.percent(&h, c, Call::SetStreamVolume(index, percent.min(100))).await
    }
    async fn set_stream_mute(&self, index: u32, muted: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::SetStreamMute(index, muted)).await
    }
    async fn move_stream_output(&self, index: u32, sink: String, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        let sink = sink.trim().to_string();
        if sink.is_empty() || sink.len() > 256 {
            return Err(Error::InvalidArgs("Укажите имя или название устройства вывода".into()));
        }
        self.done(&h, c, Call::MoveStreamOutput(index, sink)).await
    }

    // Экран и режимы
    async fn get_display(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<DisplayInfo, Error> {
        want!(self.call(&h, c, Call::GetDisplay).await, Reply::Display(d) => d)
    }
    async fn set_brightness(&self, percent: u32, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<u32, Error> {
        self.percent(&h, c, Call::SetBrightness(percent.min(100))).await
    }
    async fn change_brightness(&self, delta: i32, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<u32, Error> {
        self.percent(&h, c, Call::ChangeBrightness(delta)).await
    }
    async fn set_night_shift(&self, on: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::SetNightShift(on)).await
    }
    async fn set_dark_mode(&self, on: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::SetDarkMode(on)).await
    }
    async fn set_do_not_disturb(&self, on: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::SetDoNotDisturb(on)).await
    }
    async fn get_dock_edge(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<String, Error> {
        want!(self.call(&h, c, Call::GetDockEdge).await, Reply::DockEdge(edge) => edge)
    }
    async fn set_dock_edge(&self, edge: String, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        let edge = crate::theme::DockEdge::parse(&edge)
            .ok_or_else(|| Error::InvalidArgs("Положение Dock: left, bottom или right".into()))?;
        self.done(&h, c, Call::SetDockEdge(edge)).await
    }

    // Медиа
    async fn get_media(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<MediaInfo, Error> {
        want!(self.call(&h, c, Call::GetMedia).await, Reply::Media(m) => m)
    }
    async fn media_play_pause(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::Media(crate::mpris::Command::PlayPause)).await
    }
    async fn media_next(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::Media(crate::mpris::Command::Next)).await
    }
    async fn media_previous(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::Media(crate::mpris::Command::Previous)).await
    }

    // Система
    async fn screenshot(&self, target: String, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<String, Error> {
        want!(self.call(&h, c, Call::Screenshot(screenshot_target(&target)?)).await, Reply::Path(p) => p)
    }
    #[zbus(out_args("token", "width", "height", "png"))]
    async fn observe_window(&self, id: u64, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(u64, u32, u32, Vec<u8>), Error> {
        want!(self.call(&h, c, Call::ObserveWindow(id)).await, Reply::WindowImage(t, w, h, png) => (t, w, h, png))
    }
    async fn window_click(&self, token: u64, x: u32, y: u32, button: u32, count: u32, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        if !(1..=3).contains(&button) || !(1..=2).contains(&count) {
            return Err(Error::InvalidArgs("Кнопка: 1–3, число нажатий: 1–2".into()));
        }
        self.done(&h, c, Call::WindowInput(token, Input::Click { x, y, button, count })).await
    }
    async fn window_scroll(&self, token: u64, x: u32, y: u32, steps: i32, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        if steps == 0 || !(-12..=12).contains(&steps) {
            return Err(Error::InvalidArgs("Прокрутка: от −12 до 12 шагов".into()));
        }
        self.done(&h, c, Call::WindowInput(token, Input::Scroll { x, y, steps })).await
    }
    async fn window_key(&self, token: u64, keys: Vec<u32>, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        super::automation::validate_keys(&keys)?;
        self.done(&h, c, Call::WindowInput(token, Input::Key(keys))).await
    }
    async fn window_paste(&self, token: u64, text: String, keys: Vec<u32>, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        if text.is_empty() || text.len() > 65_536 {
            return Err(Error::InvalidArgs("Вставляемый текст: от 1 до 65536 байт".into()));
        }
        super::automation::validate_keys(&keys)?;
        self.done(&h, c, Call::WindowInput(token, Input::Paste(text, keys))).await
    }
    async fn lock_screen(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::LockScreen).await
    }
    async fn suspend(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::Suspend).await
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        feather_api::VERSION
    }
    #[zbus(property)]
    fn compositor_version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
    async fn stop_larvis_desktop(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::StopLarvisDesktop).await
    }
    /// (запущен, показан)
    async fn get_larvis_desktop(&self, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(bool, bool), Error> {
        want!(self.call(&h, c, Call::GetLarvisDesktop).await, Reply::LarvisDesktop(running, shown) => (running, shown))
    }
    async fn show_larvis_desktop(&self, show: bool, #[zbus(header)] h: Header<'_>, #[zbus(connection)] c: &Connection) -> Result<(), Error> {
        self.done(&h, c, Call::ShowLarvisDesktop(show)).await
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

/// `org.feather.ScreenCast1`: приватный интерфейс потоков демонстрации экрана строго для
/// портала Пёрышка (`org.freedesktop.impl.portal.desktop.feather`). Любой сторонний процесс на
/// сессионной шине отсекается по владельцу имени шины и исполняемому файлу `feather-de --file-portal`.
pub struct Cast {
    requests: channel::SyncSender<Request>,
    stream_owners: Mutex<HashMap<u64, String>>,
}

#[zbus::interface(name = "org.feather.ScreenCast1")]
impl Cast {
    /// `cursor_mode`: 1 — без курсора, 2 — курсор в кадре. Пустой `monitor` — монитор под
    /// курсором. Ответ: id потока, узел PipeWire и логический прямоугольник монитора.
    async fn start(
        &self,
        cursor_mode: u32,
        monitor: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(u64, u32, i32, i32, i32, i32), Error> {
        let sender = crate::dbus_auth::verify_portal_backend_for_screencast(conn, &header).await?;
        if monitor.len() > 64 {
            return Err(Error::InvalidArgs("Слишком длинное имя монитора".into()));
        }
        let (stream, node, x, y, w, h) = want!(
            send_request(&self.requests, Call::CastStart(cursor_mode != 1, monitor)).await,
            Reply::Cast { stream, node, rect } => (stream, node, rect.0, rect.1, rect.2, rect.3)
        )?;
        {
            let mut owners = self.stream_owners.lock().unwrap();
            if owners.len() >= 32 {
                owners.clear();
            }
            owners.insert(stream, sender);
        }
        Ok((stream, node, x, y, w, h))
    }

    async fn stop(
        &self,
        stream: u64,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), Error> {
        let sender = crate::dbus_auth::verify_portal_backend_for_screencast(conn, &header).await?;
        {
            let mut owners = self.stream_owners.lock().unwrap();
            if let Some(owner) = owners.get(&stream) {
                if owner != &sender {
                    return Err(Error::AccessDenied(
                        "Поток демонстрации принадлежит другому экземпляру портала".into(),
                    ));
                }
            }
            owners.remove(&stream);
        }
        want!(send_request(&self.requests, Call::CastStop(stream)).await, Reply::Done => ())
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
    conn.object_server()
        .at(
            feather_api::PATH,
            Server {
                requests: requests.clone(),
                automation_enabled,
                caller_cache: Mutex::new(HashMap::new()),
            },
        )
        .await?;
    conn.object_server()
        .at(
            feather_api::CAST_PATH,
            Cast {
                requests,
                stream_owners: Mutex::new(HashMap::new()),
            },
        )
        .await?;
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
