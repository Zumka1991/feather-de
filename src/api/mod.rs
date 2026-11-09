//! API рабочего стола `org.feather.Shell1` для внешних программ (голосовой ассистент, скрипты,
//! `featherctl`). D-Bus живёт в своём потоке (`server.rs`); здесь запросы выполняются в цикле
//! событий теми же путями, что клики и клавиши оболочки, и сразу получают ответ. Ничего не
//! ждёт: снимок экрана отвечает из своего потока кодирования. Сигналы окон собираются в
//! `refresh_focus()` — через него проходит каждое открытие, закрытие и смена фокуса.
mod server;
pub(crate) mod automation;

use crate::{shell::ShellClickAction, FeatherCompositor};
use feather_api::{AppInfo, AudioInfo, DisplayInfo, Error, MediaInfo, SpaceInfo, WindowInfo};
use smithay::reexports::calloop::{channel, LoopHandle};

#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    StopLarvisDesktop,
    GetLarvisDesktop,
    ShowLarvisDesktop(bool),
    ListWindows,
    /// Окно 0 — активное
    FocusWindow(u64),
    CloseWindow(u64),
    MinimizeWindow(u64),
    MaximizeWindow(u64, bool),
    FullscreenWindow(u64, bool),
    /// Индекс стола с 0
    MoveWindowToSpace(u64, u32),
    ListSpaces,
    SwitchSpace(u32),
    MissionControl(bool),
    Launchpad(bool),
    ListApps,
    LaunchApp(String),
    ActivateApp(String),
    GetAudio,
    SetVolume(u32),
    ChangeVolume(i32),
    SetMute(bool),
    SetMicMute(bool),
    GetDisplay,
    SetBrightness(u32),
    ChangeBrightness(i32),
    SetNightShift(bool),
    SetDarkMode(bool),
    SetDoNotDisturb(bool),
    GetDockEdge,
    SetDockEdge(crate::theme::DockEdge),
    GetMedia,
    Media(crate::mpris::Command),
    Screenshot(Shot),
    ObserveWindow(u64),
    WindowInput(u64, automation::Input),
    LockScreen,
    Suspend,
    /// Демонстрация экрана для портала: показывать ли курсор, имя монитора (пусто — под курсором)
    CastStart(bool, String),
    CastStop(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shot {
    Screen,
    /// Активное окно
    Window,
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    Done,
    Windows(Vec<WindowInfo>),
    Spaces(Vec<SpaceInfo>),
    Apps(Vec<AppInfo>),
    Audio(AudioInfo),
    Display(DisplayInfo),
    DockEdge(String),
    /// Стол Ларвиса: запущен, показан
    LarvisDesktop(bool, bool),
    Media(MediaInfo),
    Percent(u32),
    Path(String),
    WindowImage(u64, u32, u32, Vec<u8>),
    /// Поток демонстрации: id, узел PipeWire, логический прямоугольник монитора
    Cast { stream: u64, node: u32, rect: (i32, i32, i32, i32) },
}

/// Одноразовый ответ; брошенный без ответа превращается у клиента в ошибку
pub struct Responder(async_channel::Sender<Result<Reply, Error>>);

impl Responder {
    pub fn send(self, result: Result<Reply, Error>) {
        let _ = self.0.try_send(result);
    }
}

pub struct Request {
    pub call: Call,
    pub reply: Responder,
}

pub enum Event {
    Opened { id: u64, app_id: String, title: String },
    Closed(u64),
    Focus(u64),
    /// Стол пользователя (с 1), число его столов, показан ли стол Ларвиса
    Space { number: u32, count: u32, larvis: bool },
}

/// Номер стола из API (с 1) → индекс
pub fn space_index(number: u32) -> Result<u32, Error> {
    number.checked_sub(1).ok_or_else(|| Error::InvalidArgs("Рабочие столы нумеруются с 1".into()))
}

pub fn screenshot_target(target: &str) -> Result<Shot, Error> {
    match target {
        "" | "screen" => Ok(Shot::Screen),
        "window" => Ok(Shot::Window),
        other => Err(Error::InvalidArgs(format!("Снимок: screen или window, а не «{other}»"))),
    }
}

pub fn app_query(id: String) -> Result<String, Error> {
    let id = id.trim().to_string();
    if id.is_empty() || id.len() > 256 {
        return Err(Error::InvalidArgs("Нужен ID приложения из ListApps".into()));
    }
    Ok(id)
}

/// Шаг громкости/яркости в процентах: не выше 100 (громкость выше 100 % сама не растёт)
/// и не ниже 0
pub fn step_percent(current: u32, delta: i32) -> u32 {
    (current as i64 + delta as i64).clamp(0, current.max(100) as i64) as u32
}

pub fn percent(ratio: f32) -> u32 {
    (ratio.max(0.0) * 100.0).round() as u32
}

/// Под блокировкой экрана: только чтение, звук, медиа, блокировка и сон (кнопки экрана
/// блокировки умеют то же)
pub fn allowed_while_locked(call: &Call) -> bool {
    matches!(
        call,
        Call::ListWindows
            | Call::ListSpaces
            | Call::ListApps
            | Call::GetAudio
            | Call::GetDisplay
            | Call::GetDockEdge
            | Call::GetLarvisDesktop
            | Call::GetMedia
            | Call::SetVolume(_)
            | Call::ChangeVolume(_)
            | Call::SetMute(_)
            | Call::SetMicMute(_)
            | Call::Media(_)
            | Call::LockScreen
            | Call::Suspend
            | Call::CastStop(_)
    )
}

pub struct Api {
    events: async_channel::Sender<Event>,
    /// Открытые (mapped) окна, о которых уже сообщили, по возрастанию id
    known: Vec<u64>,
    focus: u64,
    /// Последний сообщённый `SpaceChanged`: стол с 1, число столов, стол Ларвиса
    space: (u32, u32, bool),
    view: Option<automation::View>,
    next_view: u64,
    last_observation: Option<std::time::Instant>,
    automation_enabled: bool,
}

impl Api {
    /// `replace`: свой сеанс забирает имя у зависшего вложенного запуска
    pub fn new(handle: &LoopHandle<'static, FeatherCompositor>, replace: bool) -> Result<Self, Box<dyn std::error::Error>> {
        let (requests, source) = channel::sync_channel(32);
        handle.insert_source(source, |event, _, state| {
            if let channel::Event::Msg(request) = event {
                state.api_request(request);
            }
        })?;
        let (events, rx) = async_channel::bounded(64);
        let name = feather_api::bus_name();
        let automation_enabled = std::env::var("FEATHER_AUTOMATION").as_deref() == Ok("1");
        std::thread::Builder::new()
            .name("feather-api".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                futures_lite::future::block_on(async move {
                    if let Err(e) = server::run(requests, rx, name, replace, automation_enabled).await {
                        tracing::warn!("API: {e}");
                    }
                })
            })?;
        let next_view = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_nanos() as u64;
        Ok(Self {
            events, known: Vec::new(), focus: 0, space: (1, 1, false), view: None, next_view,
            last_observation: None, automation_enabled,
        })
    }
}

fn not_found(what: impl Into<String>) -> Error {
    Error::NotFound(what.into())
}

fn unavailable(what: impl Into<String>) -> Error {
    Error::Unavailable(what.into())
}

impl FeatherCompositor {
    pub(crate) fn api_invalidate_view(&mut self) {
        if let Some(api) = self.api.as_mut() {
            api.view = None;
        }
    }

    /// Открытия, закрытия и смена фокуса с прошлого вызова → сигналы (из `refresh_focus`)
    pub fn api_sync(&mut self) {
        let Some(api) = self.api.as_mut() else { return };
        // Larvis's desktop is a desktop, not a window of the user
        let mut now: Vec<u64> = self.windows.iter().filter(|w| w.mapped && !w.larvis).map(|w| w.id).collect();
        now.sort_unstable();
        if now != api.known {
            for id in api.known.iter().filter(|id| now.binary_search(id).is_err()) {
                let _ = api.events.try_send(Event::Closed(*id));
            }
            for w in self.windows.iter().filter(|w| w.mapped && !w.larvis && api.known.binary_search(&w.id).is_err()) {
                let _ = api.events.try_send(Event::Opened { id: w.id, app_id: w.app_id.clone(), title: w.title.clone() });
            }
            api.known = now;
        }
        let space = (self.spaces.current + 1, self.spaces.count, self.larvis_shown);
        if space != api.space {
            api.space = space;
            let _ = api.events.try_send(Event::Space { number: space.0, count: space.1, larvis: space.2 });
        }
        let focus = self.windows.iter().rfind(|w| w.activated && !w.larvis).map_or(0, |w| w.id);
        if focus != api.focus {
            api.view = None;
            api.focus = focus;
            let _ = api.events.try_send(Event::Focus(focus));
        }
    }

    fn api_request(&mut self, request: Request) {
        let Request { call, reply } = request;
        if self.lock.active && !allowed_while_locked(&call) {
            return reply.send(Err(unavailable("Экран заблокирован")));
        }
        match call {
            Call::Screenshot(target) => return self.api_screenshot(target, reply),
            Call::ObserveWindow(id) => return self.api_observe(id, reply),
            // Answered once PipeWire has created the node
            Call::CastStart(cursor, monitor) => return self.cast_start(cursor, &monitor, reply),
            Call::CastStop(id) => {
                self.cast_stop(id);
                return reply.send(Ok(Reply::Done));
            }
            _ => {}
        }
        let result = self.api_call(call);
        if result.is_ok() {
            self.needs_redraw = true;
        }
        reply.send(result);
    }

    /// Индекс открытого окна; 0 — верхнее видимое (активное)
    fn api_window(&self, id: u64) -> Result<usize, Error> {
        if id == 0 {
            return self.windows.iter().rposition(|w| w.is_visible() && !w.larvis).ok_or_else(|| not_found("Нет активного окна"));
        }
        self.windows.iter().position(|w| w.id == id && w.mapped && !w.larvis).ok_or_else(|| not_found(format!("Нет окна {id}")))
    }

    fn api_app(&self, query: &str) -> Result<usize, Error> {
        let r = &self.app_registry;
        r.index_of(query)
            .or_else(|| r.app_for_app_id(query))
            .or_else(|| r.apps.iter().position(|a| a.name.to_lowercase() == query.to_lowercase()))
            .ok_or_else(|| not_found(format!("Нет приложения «{query}»")))
    }

    fn api_call(&mut self, call: Call) -> Result<Reply, Error> {
        use crate::controls::Control;
        match call {
            Call::StopLarvisDesktop if self.larvis_desktop => self.loop_signal.stop(),
            Call::StopLarvisDesktop => return Err(unavailable("Это основной сеанс Feather")),
            Call::GetLarvisDesktop => {
                return Ok(Reply::LarvisDesktop(self.larvis_window().is_some(), self.larvis_shown));
            }
            Call::ShowLarvisDesktop(_) if self.larvis_desktop => {
                return Err(unavailable("Это сам стол Ларвиса"));
            }
            Call::ShowLarvisDesktop(show) => {
                if !self.set_larvis_shown(show) && show {
                    return Err(unavailable("Стол Ларвиса не запущен"));
                }
            }
            Call::ListWindows => {
                let list = self
                    .windows
                    .iter()
                    .rev()
                    .filter(|w| w.mapped && !w.larvis)
                    .map(|w| WindowInfo {
                        id: w.id,
                        app_id: w.app_id.clone(),
                        app_name: match self.app_key(w) {
                            crate::shell::AppKey::App(i) => self.app_registry.apps[i].name.clone(),
                            _ => String::new(),
                        },
                        title: w.title.clone(),
                        // 0: on the Подложка, over every desktop
                        space: if w.space == crate::scratch::SCRATCH_SPACE { 0 } else { w.space + 1 },
                        focused: w.activated,
                        minimized: w.minimized,
                        maximized: w.maximized,
                        fullscreen: w.fullscreen,
                    })
                    .collect();
                return Ok(Reply::Windows(list));
            }
            Call::FocusWindow(id) => {
                let id = self.windows[self.api_window(id)?].id;
                let minimized = self.windows.iter().any(|w| w.id == id && w.minimized);
                self.execute_shell_action(if minimized { ShellClickAction::RestoreMinimizedWindow(id) } else { ShellClickAction::FocusWindow(id) });
            }
            Call::CloseWindow(id) => {
                let i = self.api_window(id)?;
                match self.windows[i].backend {
                    crate::window::WindowBackend::Wayland(ref t) => t.send_close(),
                    crate::window::WindowBackend::Builtin(_) => {
                        self.windows.remove(i);
                        self.refresh_focus();
                    }
                }
                self.shell.topbar_dirty = true;
                self.shell.dock_dirty = true;
            }
            Call::MinimizeWindow(id) => {
                let i = self.api_window(id)?;
                self.windows[i].minimized = true;
                self.shell.dock_dirty = true;
                self.refresh_focus();
            }
            Call::MaximizeWindow(id, on) => {
                let i = self.api_window(id)?;
                let r = self.window_monitor(i);
                let w = &mut self.windows[i];
                w.resize_anchor = None;
                w.maximize_on(on, r);
            }
            Call::FullscreenWindow(id, on) => {
                let i = self.api_window(id)?;
                if self.windows[i].fullscreen != on {
                    let r = self.window_monitor(i);
                    self.windows[i].resize_anchor = None;
                    self.set_window_fullscreen(i, on, r);
                }
            }
            Call::MoveWindowToSpace(id, space) => {
                let i = self.api_window(id)?;
                let count = self.spaces.count;
                // One past the last desktop adds a desktop, like Ctrl+Alt+→ from the last one
                if space > count || (space == count && count >= crate::spaces::MAX_SPACES) {
                    return Err(not_found(format!("Нет рабочего стола {}: их {count}", space + 1)));
                }
                self.spaces.count = count.max(space + 1);
                self.windows[i].space = space;
                self.spaces = crate::spaces::prune(self.spaces, |s| self.windows.iter().any(|w| w.mapped && w.space == s));
                self.apply_space_visibility();
                self.dock_model_dirty = true;
                self.pointer_focus_dirty = true;
                if self.mission_open() {
                    self.relayout_mission();
                }
                self.refresh_focus();
            }
            Call::ListSpaces => {
                let list = (0..self.spaces.count)
                    .map(|s| SpaceInfo {
                        number: s + 1,
                        current: s == self.spaces.current,
                        windows: self.windows.iter().filter(|w| w.mapped && w.space == s).count() as u32,
                    })
                    .collect();
                return Ok(Reply::Spaces(list));
            }
            Call::SwitchSpace(space) => {
                if space >= self.spaces.count {
                    return Err(not_found(format!("Нет рабочего стола {}: их {}", space + 1, self.spaces.count)));
                }
                self.switch_space(space);
                if self.mission_open() {
                    self.relayout_mission();
                }
            }
            Call::MissionControl(show) => {
                if show != self.mission_open() {
                    if show { self.open_mission() } else { self.close_mission(None) }
                }
            }
            Call::Launchpad(show) => {
                if show != self.shell.launchpad.is_some() {
                    self.toggle_launchpad();
                    self.pointer_focus_dirty = true;
                }
            }
            Call::ListApps => {
                let running: std::collections::HashSet<usize> = self
                    .windows
                    .iter()
                    .filter(|w| w.mapped)
                    .filter_map(|w| match self.app_key(w) {
                        crate::shell::AppKey::App(i) => Some(i),
                        _ => None,
                    })
                    .collect();
                let list = self
                    .app_registry
                    .launchpad_apps("")
                    .into_iter()
                    .map(|i| {
                        let a = &self.app_registry.apps[i];
                        AppInfo { id: a.id.clone(), name: a.name.clone(), description: a.subtitle.clone(), running: running.contains(&i) }
                    })
                    .collect();
                return Ok(Reply::Apps(list));
            }
            Call::LaunchApp(query) => {
                let i = self.api_app(&query)?;
                self.launch_app_instance(i);
            }
            Call::ActivateApp(query) => {
                let i = self.api_app(&query)?;
                self.activate_app(crate::shell::AppKey::App(i));
            }
            Call::GetAudio => {
                let a = &self.shell.cc.audio;
                let (out, input) = (a.current(), a.current_source());
                return Ok(Reply::Audio(AudioInfo {
                    available: out.is_some(),
                    output: out.map(|s| s.label.clone()).unwrap_or_default(),
                    volume: out.map_or(0, |s| percent(s.volume())),
                    muted: out.is_some_and(|s| s.muted),
                    mic_available: input.is_some(),
                    input: input.map(|s| s.label.clone()).unwrap_or_default(),
                    mic_volume: input.map_or(0, |s| percent(s.volume())),
                    mic_muted: input.is_some_and(|s| s.muted),
                }));
            }
            Call::SetVolume(_) | Call::ChangeVolume(_) | Call::SetMute(_) => {
                let sink = self.shell.cc.audio.current().cloned().ok_or_else(|| unavailable("Нет устройства вывода звука"))?;
                let controls = self.controls.as_ref().ok_or_else(|| unavailable("Служба звука недоступна"))?;
                let now = percent(sink.volume());
                let (value, muted) = match call {
                    Call::SetVolume(p) => (Some(p), sink.muted && p == 0),
                    Call::ChangeVolume(d) => (Some(step_percent(now, d)), sink.muted && d <= 0),
                    Call::SetMute(m) => (None, m),
                    _ => unreachable!(),
                };
                if let Some(v) = value.filter(|v| *v != now) {
                    controls.send(Control::Volume { sink: sink.name.clone(), value: v as f32 / 100.0 }).map_err(unavailable)?;
                }
                if muted != sink.muted {
                    controls.send(Control::Mute { sink: sink.name.clone(), muted }).map_err(unavailable)?;
                }
                // The same feedback as the volume keys, from the expected state (the worker
                // reports the real one moments later)
                let mut shown = self.shell.cc.audio.clone();
                if let Some(s) = shown.sinks.iter_mut().find(|s| s.name == sink.name) {
                    if let Some(v) = value {
                        let level = (v as f32 / 100.0 * 65536.0).round() as u32;
                        s.channels.iter_mut().for_each(|c| *c = level);
                    }
                    s.muted = muted;
                }
                self.show_audio_feedback(&shown);
                return Ok(match value {
                    Some(v) => Reply::Percent(v),
                    None => Reply::Done,
                });
            }
            Call::SetMicMute(muted) => {
                let source = self.shell.cc.audio.current_source().ok_or_else(|| unavailable("Нет микрофона"))?.name.clone();
                let controls = self.controls.as_ref().ok_or_else(|| unavailable("Служба звука недоступна"))?;
                controls.send(Control::InputMute { source, muted }).map_err(unavailable)?;
            }
            Call::GetDisplay => {
                return Ok(Reply::Display(DisplayInfo {
                    brightness: self.shell.cc.brightness.filter(|_| self.brightness.is_some()).map_or(-1, |b| percent(b) as i32),
                    night_shift: self.night.active(),
                    dark_mode: crate::theme::appearance() == crate::theme::Appearance::Dark,
                    do_not_disturb: self.shell.dnd_active(),
                }));
            }
            Call::SetBrightness(_) | Call::ChangeBrightness(_) => {
                let now = self.shell.cc.brightness.filter(|_| self.brightness.is_some()).ok_or_else(|| unavailable("Яркость этого экрана не управляется"))?;
                let value = match call {
                    Call::SetBrightness(p) => p,
                    Call::ChangeBrightness(d) => step_percent(percent(now), d),
                    _ => unreachable!(),
                };
                self.execute_shell_action(ShellClickAction::SetBrightness(value as f32 / 100.0));
                return Ok(Reply::Percent(value));
            }
            Call::SetNightShift(on) => {
                if self.night.active() != on {
                    self.apply_system_setting(crate::system_settings::Action::NightNow);
                }
            }
            Call::SetDarkMode(on) => {
                if (crate::theme::appearance() == crate::theme::Appearance::Dark) != on {
                    self.toggle_appearance();
                }
            }
            Call::SetDoNotDisturb(on) => {
                if self.shell.dnd_active() != on {
                    self.apply_system_setting(crate::system_settings::Action::DoNotDisturb);
                }
            }
            Call::GetDockEdge => return Ok(Reply::DockEdge(crate::theme::dock_edge().id().into())),
            Call::SetDockEdge(edge) => {
                if crate::theme::dock_edge() != edge {
                    self.apply_system_setting(crate::system_settings::Action::DockEdge(edge));
                }
            }
            Call::GetMedia => {
                let m = self.shell.cc.media.as_ref();
                return Ok(Reply::Media(MediaInfo {
                    available: m.is_some(),
                    player: m.map(|p| p.identity.clone()).unwrap_or_default(),
                    title: m.map(|p| p.title.clone()).unwrap_or_default(),
                    artist: m.map(|p| p.artist.clone()).unwrap_or_default(),
                    playing: m.is_some_and(|p| p.playing),
                }));
            }
            Call::Media(command) => {
                let mpris = self.mpris.as_ref().ok_or_else(|| unavailable("Служба MPRIS недоступна"))?;
                if self.shell.cc.media.is_none() {
                    return Err(not_found("Нет плеера"));
                }
                mpris.send(command);
            }
            Call::LockScreen => self.lock_session(),
            Call::Suspend => self.sleep_system(),
            Call::WindowInput(token, input) => self.api_window_input(token, input)?,
            Call::Screenshot(_) | Call::ObserveWindow(_) | Call::CastStart(..) | Call::CastStop(_) => {
                unreachable!("answered in api_request")
            }
        }
        Ok(Reply::Done)
    }

    fn api_screenshot(&mut self, target: Shot, reply: Responder) {
        let target = match target {
            Shot::Screen => crate::screenshot::Target::Screen,
            Shot::Window => match self.window_target(|_| true) {
                Some(t) => t,
                None => return reply.send(Err(not_found("Нет активного окна"))),
            },
        };
        self.take_screenshot_for(target, Some(reply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_checked_before_reaching_the_loop() {
        assert_eq!(space_index(1).unwrap(), 0);
        assert_eq!(space_index(3).unwrap(), 2);
        assert!(matches!(space_index(0), Err(Error::InvalidArgs(_))));
        assert_eq!(screenshot_target("").unwrap(), Shot::Screen);
        assert_eq!(screenshot_target("window").unwrap(), Shot::Window);
        assert!(matches!(screenshot_target("area"), Err(Error::InvalidArgs(_))));
        assert_eq!(app_query("  firefox ".into()).unwrap(), "firefox");
        assert!(app_query("   ".into()).is_err());
        assert!(app_query("x".repeat(300)).is_err());
    }

    #[test]
    fn steps_stay_in_range_and_never_amplify() {
        assert_eq!(step_percent(50, 5), 55);
        assert_eq!(step_percent(98, 5), 100);
        assert_eq!(step_percent(3, -5), 0);
        assert_eq!(step_percent(120, 5), 120, "over-amplified volume does not grow");
        assert_eq!(step_percent(120, -5), 115);
        assert_eq!(percent(0.554), 55);
        assert_eq!(percent(-0.1), 0);
    }

    #[test]
    fn the_lock_screen_keeps_windows_and_apps_out_of_reach() {
        for call in [Call::FocusWindow(0), Call::LaunchApp("x".into()), Call::SwitchSpace(0), Call::Screenshot(Shot::Screen), Call::SetDarkMode(true)] {
            assert!(!allowed_while_locked(&call), "{call:?}");
        }
        for call in [Call::ListWindows, Call::ChangeVolume(5), Call::Media(crate::mpris::Command::Next), Call::LockScreen, Call::Suspend] {
            assert!(allowed_while_locked(&call), "{call:?}");
        }
    }

    /// Every method on a real bus (dbus-run-session): arguments become the right `Call`, the
    /// loop's reply comes back typed, a dropped reply is an error, and a running owner keeps
    /// the name.
    #[test]
    #[ignore = "requires an isolated session bus: dbus-run-session -- cargo test -- --include-ignored"]
    fn server_routes_every_method_on_a_real_bus() {
        use std::time::{Duration, Instant};
        let name = "org.feather.Shell1.RouteTest";
        let (requests, source) = channel::sync_channel::<Request>(8);
        let (events, rx) = async_channel::bounded(8);
        std::thread::spawn(move || futures_lite::future::block_on(server::run(requests, rx, name.into(), false, true)).unwrap());
        // The fake event loop: records each call and answers like the compositor would
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Call>::new()));
        let seen = calls.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                let Ok(Request { call, reply }) = source.try_recv() else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                seen.lock().unwrap().push(call.clone());
                let result = match call {
                    Call::ListWindows => Some(Ok(Reply::Windows(vec![WindowInfo { id: 7, title: "Окно".into(), space: 1, focused: true, ..Default::default() }]))),
                    Call::ListSpaces => Some(Ok(Reply::Spaces(vec![SpaceInfo { number: 1, current: true, windows: 1 }]))),
                    Call::GetAudio => Some(Ok(Reply::Audio(AudioInfo { available: true, volume: 40, ..Default::default() }))),
                    Call::SetVolume(p) => Some(Ok(Reply::Percent(p))),
                    Call::ChangeVolume(d) => Some(Ok(Reply::Percent(step_percent(40, d)))),
                    Call::Screenshot(_) => Some(Ok(Reply::Path("/tmp/shot.png".into()))),
                    Call::FocusWindow(404) => Some(Err(Error::NotFound("Нет окна 404".into()))),
                    // A request the loop could not finish: the responder is dropped unanswered
                    Call::CloseWindow(_) => None,
                    _ => Some(Ok(Reply::Done)),
                };
                if let Some(result) = result {
                    reply.send(result);
                }
            }
        });
        let conn = zbus::blocking::Connection::session().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let shell = loop {
            let p = feather_api::Shell1ProxyBlocking::builder(&conn).destination(name).unwrap().build().unwrap();
            if p.version().is_ok() {
                break p;
            }
            assert!(Instant::now() < deadline, "server never took the name");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(shell.version().unwrap(), feather_api::VERSION);
        assert_eq!(shell.compositor_version().unwrap(), env!("CARGO_PKG_VERSION"));
        assert_eq!(shell.list_windows().unwrap()[0].id, 7);
        assert_eq!(shell.list_spaces().unwrap().len(), 1);
        assert_eq!(shell.get_audio().unwrap().volume, 40);
        assert_eq!(shell.set_volume(250).unwrap(), 100, "clamped before the loop");
        assert_eq!(shell.change_volume(-15).unwrap(), 25);
        assert_eq!(shell.screenshot("window").unwrap(), "/tmp/shot.png");
        shell.switch_space(2).unwrap();
        shell.move_window_to_space(7, 3).unwrap();
        shell.maximize_window(0, true).unwrap();
        shell.launch_app(" firefox ").unwrap();
        shell.set_dark_mode(true).unwrap();
        shell.media_next().unwrap();
        shell.suspend().unwrap();
        let err = shell.focus_window(404).unwrap_err();
        assert!(matches!(err, Error::NotFound(ref m) if m == "Нет окна 404"), "{err:?}");
        assert!(matches!(shell.close_window(1), Err(Error::Failed(_))), "a dropped reply is an error");
        assert!(matches!(shell.switch_space(0), Err(Error::InvalidArgs(_))), "refused on the D-Bus thread");
        assert!(matches!(shell.screenshot("area"), Err(Error::InvalidArgs(_))));
        let calls = calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            [
                Call::ListWindows,
                Call::ListSpaces,
                Call::GetAudio,
                Call::SetVolume(100),
                Call::ChangeVolume(-15),
                Call::Screenshot(Shot::Window),
                Call::SwitchSpace(1),
                Call::MoveWindowToSpace(7, 2),
                Call::MaximizeWindow(0, true),
                Call::LaunchApp("firefox".into()),
                Call::SetDarkMode(true),
                Call::Media(crate::mpris::Command::Next),
                Call::Suspend,
                Call::FocusWindow(404),
                Call::CloseWindow(1),
            ]
        );
        // Signals reach subscribers
        let mut opened = shell.receive_window_opened().unwrap();
        let mut focus = shell.receive_focus_changed().unwrap();
        events.try_send(Event::Opened { id: 9, app_id: "foot".into(), title: "Терминал".into() }).unwrap();
        events.try_send(Event::Focus(9)).unwrap();
        let o = opened.next().unwrap();
        let a = o.args().unwrap();
        assert_eq!((a.id, a.app_id.as_str(), a.title.as_str()), (9, "foot", "Терминал"));
        assert_eq!(focus.next().unwrap().args().unwrap().id, 9);
        // A second compositor on the same bus leaves the owner alone
        let owner = zbus::blocking::fdo::DBusProxy::new(&conn).unwrap().get_name_owner(name.try_into().unwrap()).unwrap();
        let (r2, _s2) = channel::sync_channel::<Request>(1);
        let (_e2, rx2) = async_channel::bounded(1);
        drop(_e2);
        futures_lite::future::block_on(server::run(r2, rx2, name.into(), false, false)).unwrap();
        let after = zbus::blocking::fdo::DBusProxy::new(&conn).unwrap().get_name_owner(name.try_into().unwrap()).unwrap();
        assert_eq!(owner, after);
    }
}
