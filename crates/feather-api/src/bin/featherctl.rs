//! featherctl — управление Пёрышком из терминала, скриптов и горячих клавиш через
//! `org.feather.Shell1`. `featherctl help` — список команд.
use feather_api::{connect, Error, Shell1ProxyBlocking};

const HELP: &str = "featherctl — управление рабочим столом Пёрышка (org.feather.Shell1)

Использование: featherctl [--json] КОМАНДА

Окна (ID — число из `window list` или `active`):
  window list                        список окон
  window focus|close|minimize ID     показать / закрыть (опасно) / свернуть
  window maximize|fullscreen ID [on|off|toggle]
  window pip [ID] [status|on|off|ghost|toggle]  «Картинка в картинке» / «Сквозь курсор»
  window move ID СТОЛ                перенести на рабочий стол (с 1)
Рабочие столы:
  space [list]                       список столов
  space N                            перейти на стол N
  space-mode [free|tile|tape|next] [N] режим раскладки стола (N с 1; без N — текущий)
  mission [on|off]                   Mission Control
  launchpad [on|off]                 Launchpad
  menu-hud [open|close|toggle]       HUD-поиск по командам меню (Super + /)
  shelf [status|on|off|toggle|clear|add ПУТЬ...] полка перетаскивания
  larvis [status|on|off|toggle]      стол Ларвиса вместо твоих столов
Приложения:
  app list                           приложения из .desktop
  app launch ID                      новый экземпляр
  app open ID                        показать открытое или запустить
Звук и экран:
  volume [N|+N|-N|mute|unmute|toggle]
  mic [mute|unmute|toggle]
  audio streams                      активные звуковые потоки программ
  audio app-volume INDEX 0..100      громкость потока программы
  audio app-mute INDEX [on|off|toggle]
  audio app-output INDEX УСТРОЙСТВО  направить поток программы на устройство вывода
  brightness [N|+N|-N]
  night|dark|dnd [on|off|toggle]     ночной режим / тёмная тема / «Не беспокоить»
  display                            яркость и режимы
Медиа: media [status|play-pause|next|previous]
Система:
  screenshot [screen|window]         снимок, печатает путь к файлу
  lock                               заблокировать экран
  suspend                            сон (опасно)
  version                            версии API и Пёрышка
  watch                              печатать события окон

Другое имя сервиса: FEATHER_API_NAME=org.feather.Shell1.Test featherctl …";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Switch {
    On,
    Off,
    Toggle,
}

impl Switch {
    fn resolve(self, current: bool) -> bool {
        match self {
            Self::On => true,
            Self::Off => false,
            Self::Toggle => !current,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    Show,
    Set(u32),
    Change(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Night,
    Dark,
    Dnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipAction {
    Status,
    On,
    Off,
    GhostToggle,
    Toggle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Command {
    Help,
    Version,
    Watch,
    Windows,
    Focus(u64),
    Close(u64),
    Minimize(u64),
    Maximize(u64, Switch),
    Fullscreen(u64, Switch),
    WindowPip(u64, PipAction),
    MoveWindow(u64, u32),
    Spaces,
    SwitchSpace(u32),
    GetSpaceMode(u32),
    SetSpaceMode(u32, String),
    Mission(bool),
    Launchpad(bool),
    MenuHud(Switch),
    ShelfStatus,
    ShelfShow(Switch),
    ShelfClear,
    ShelfAdd(Vec<String>),
    LarvisStatus,
    Larvis(Switch),
    Apps,
    Launch(String),
    Activate(String),
    Volume(Level),
    Mute(Switch),
    MicMute(Switch),
    AudioStreams,
    StreamVolume(u32, u32),
    StreamMute(u32, Switch),
    StreamOutput(u32, String),
    Brightness(Level),
    Mode(Mode, Switch),
    Display,
    MediaStatus,
    PlayPause,
    Next,
    Previous,
    Screenshot(String),
    Lock,
    Suspend,
}

fn bad(msg: impl Into<String>) -> String {
    msg.into()
}

fn window_id(arg: Option<&str>) -> Result<u64, String> {
    match arg {
        None => Err(bad("Нужен ID окна (число из `window list` или active)")),
        Some("active" | "focused") => Ok(0),
        Some(s) => s.parse().map_err(|_| bad(format!("Неверный ID окна: {s}"))),
    }
}

fn pip_action(arg: Option<&str>) -> Result<PipAction, String> {
    match arg {
        None | Some("toggle") => Ok(PipAction::Toggle),
        Some("status") => Ok(PipAction::Status),
        Some("on" | "1" | "true" | "вкл") => Ok(PipAction::On),
        Some("off" | "0" | "false" | "выкл") => Ok(PipAction::Off),
        Some("ghost" | "click-through") => Ok(PipAction::GhostToggle),
        Some(s) => Err(bad(format!("Ожидалось status, on, off, ghost или toggle, а не {s}"))),
    }
}

fn switch(arg: Option<&str>, default: Switch) -> Result<Switch, String> {
    match arg {
        None => Ok(default),
        Some("on" | "1" | "true" | "вкл") => Ok(Switch::On),
        Some("off" | "0" | "false" | "выкл") => Ok(Switch::Off),
        Some("toggle") => Ok(Switch::Toggle),
        Some(s) => Err(bad(format!("Ожидалось on, off или toggle, а не {s}"))),
    }
}

fn number(arg: &str, what: &str) -> Result<u32, String> {
    arg.parse().map_err(|_| bad(format!("Неверное значение {what}: {arg}")))
}

fn level(arg: Option<&str>) -> Result<Level, String> {
    let Some(s) = arg else { return Ok(Level::Show) };
    let parsed = if let Some(d) = s.strip_prefix('+') {
        d.parse::<i32>().map(Level::Change)
    } else if s.starts_with('-') {
        s.parse::<i32>().map(Level::Change)
    } else {
        s.trim_end_matches('%').parse::<u32>().map(Level::Set)
    };
    parsed.map_err(|_| bad(format!("Неверный уровень: {s} (N, +N или -N)")))
}

fn mute(arg: Option<&str>) -> Result<Switch, String> {
    match arg {
        Some("mute") => Ok(Switch::On),
        Some("unmute") => Ok(Switch::Off),
        Some("toggle") | None => Ok(Switch::Toggle),
        Some(s) => Err(bad(format!("Ожидалось mute, unmute или toggle, а не {s}"))),
    }
}

fn parse_space_mode(rest: &[&str]) -> Result<Command, String> {
    match rest {
        [] => Ok(Command::GetSpaceMode(0)),
        [one] => {
            if let Ok(n) = one.parse::<u32>() {
                if n == 0 {
                    return Err(bad("Рабочие столы нумеруются с 1"));
                }
                return Ok(Command::GetSpaceMode(n));
            }
            let m = one.to_ascii_lowercase();
            if !matches!(m.as_str(), "free" | "tile" | "tape" | "next") {
                return Err(bad(format!("Режим стола: free, tile, tape или next, а не {one}")));
            }
            Ok(Command::SetSpaceMode(0, m))
        }
        [mode, n] => {
            let m = mode.to_ascii_lowercase();
            if !matches!(m.as_str(), "free" | "tile" | "tape" | "next") {
                return Err(bad(format!("Режим стола: free, tile, tape или next, а не {mode}")));
            }
            let space = number(n, "стола")?;
            if space == 0 {
                return Err(bad("Рабочие столы нумеруются с 1"));
            }
            Ok(Command::SetSpaceMode(space, m))
        }
        _ => Err(bad(format!("Лишний аргумент: {}", rest[2]))),
    }
}

/// Разбор аргументов без `--json`
fn parse(args: &[&str]) -> Result<Command, String> {
    let arg = |i: usize| args.get(i).copied();
    let no_more = |n: usize, c: Command| if args.len() > n { Err(bad(format!("Лишний аргумент: {}", args[n]))) } else { Ok(c) };
    let Some(&head) = args.first() else { return Ok(Command::Help) };
    match head {
        "help" | "-h" | "--help" => Ok(Command::Help),
        "version" | "--version" => no_more(1, Command::Version),
        "watch" => no_more(1, Command::Watch),
        "windows" => no_more(1, Command::Windows),
        "window" => {
            let id = || window_id(arg(2));
            match arg(1) {
                None | Some("list") => no_more(2, Command::Windows),
                Some("focus") => no_more(3, Command::Focus(id()?)),
                Some("close") => no_more(3, Command::Close(id()?)),
                Some("minimize") => no_more(3, Command::Minimize(id()?)),
                Some("maximize") => no_more(4, Command::Maximize(id()?, switch(arg(3), Switch::Toggle)?)),
                Some("fullscreen") => no_more(4, Command::Fullscreen(id()?, switch(arg(3), Switch::Toggle)?)),
                Some("pip") => match (arg(2), arg(3)) {
                    (None, _) => Ok(Command::WindowPip(0, PipAction::Toggle)),
                    (Some(mode @ ("status" | "on" | "off" | "ghost" | "click-through" | "toggle")), None) => {
                        Ok(Command::WindowPip(0, pip_action(Some(mode))?))
                    }
                    (Some(raw_id), mode) => {
                        no_more(4, Command::WindowPip(window_id(Some(raw_id))?, pip_action(mode)?))
                    }
                },
                Some("move") => {
                    let space = arg(3).ok_or_else(|| bad("Нужен номер стола"))?;
                    no_more(4, Command::MoveWindow(id()?, number(space, "стола")?))
                }
                Some(s) => Err(bad(format!("Неизвестная команда окна: {s}"))),
            }
        }
        "spaces" => no_more(1, Command::Spaces),
        "space" => match arg(1) {
            None | Some("list") => no_more(2, Command::Spaces),
            Some("mode") => parse_space_mode(&args[2..]),
            Some(n) => no_more(2, Command::SwitchSpace(number(n, "стола")?)),
        },
        "space-mode" => parse_space_mode(&args[1..]),
        "mission" => no_more(2, Command::Mission(switch(arg(1), Switch::On)? != Switch::Off)),
        "launchpad" => no_more(2, Command::Launchpad(switch(arg(1), Switch::On)? != Switch::Off)),
        "menu-hud" | "hud" => match arg(1) {
            Some("open") => no_more(2, Command::MenuHud(Switch::On)),
            Some("close") => no_more(2, Command::MenuHud(Switch::Off)),
            other => no_more(2, Command::MenuHud(switch(other, Switch::Toggle)?)),
        },
        "shelf" => match arg(1) {
            None | Some("status" | "list") => no_more(2, Command::ShelfStatus),
            Some("clear") => no_more(2, Command::ShelfClear),
            Some("open") => no_more(2, Command::ShelfShow(Switch::On)),
            Some("close") => no_more(2, Command::ShelfShow(Switch::Off)),
            Some("add") => {
                if args.len() < 3 {
                    return Err(bad("Укажите хотя бы один путь: featherctl shelf add ПУТЬ..."));
                }
                let cwd = std::env::current_dir().unwrap_or_default();
                let paths = args[2..]
                    .iter()
                    .map(|s| {
                        let p = std::path::PathBuf::from(s);
                        let abs = if p.is_absolute() { p } else { cwd.join(p) };
                        abs.to_string_lossy().into_owned()
                    })
                    .collect();
                Ok(Command::ShelfAdd(paths))
            }
            other => no_more(2, Command::ShelfShow(switch(other, Switch::Toggle)?)),
        },
        "apps" => no_more(1, Command::Apps),
        "app" => match (arg(1), arg(2)) {
            (None | Some("list"), _) => no_more(2, Command::Apps),
            (Some("launch"), Some(id)) => no_more(3, Command::Launch(id.into())),
            (Some("open"), Some(id)) => no_more(3, Command::Activate(id.into())),
            (Some("launch" | "open"), None) => Err(bad("Нужен ID приложения из `app list`")),
            (Some(s), _) => Err(bad(format!("Неизвестная команда приложения: {s}"))),
        },
        "volume" => match arg(1) {
            Some(s @ ("mute" | "unmute" | "toggle")) => no_more(2, Command::Mute(mute(Some(s))?)),
            other => no_more(2, Command::Volume(level(other)?)),
        },
        "mic" => no_more(2, Command::MicMute(mute(arg(1))?)),
        "audio" => match arg(1) {
            None | Some("streams" | "list") => no_more(2, Command::AudioStreams),
            Some("app-volume") => {
                let idx_raw = arg(2).ok_or_else(|| bad("Нужен индекс потока из `audio streams`"))?;
                let vol_raw = arg(3).ok_or_else(|| bad("Нужна громкость 0..100"))?;
                let idx = number(idx_raw, "потока")?;
                let vol = vol_raw
                    .trim_end_matches('%')
                    .parse::<u32>()
                    .map_err(|_| bad(format!("Неверная громкость: {vol_raw}")))?;
                if vol > 100 {
                    return Err(bad("Громкость потока — от 0 до 100"));
                }
                no_more(4, Command::StreamVolume(idx, vol))
            }
            Some("app-mute") => {
                let idx_raw = arg(2).ok_or_else(|| bad("Нужен индекс потока из `audio streams`"))?;
                let idx = number(idx_raw, "потока")?;
                let sw = match arg(3) {
                    Some(m @ ("mute" | "unmute")) => mute(Some(m))?,
                    other => switch(other, Switch::Toggle)?,
                };
                no_more(4, Command::StreamMute(idx, sw))
            }
            Some("app-output") => {
                let idx_raw = arg(2).ok_or_else(|| bad("Нужен индекс потока из `audio streams`"))?;
                let idx = number(idx_raw, "потока")?;
                if args.len() < 4 {
                    return Err(bad("Нужно устройство вывода (имя или описание)"));
                }
                let sink = args[3..].join(" ");
                if sink.trim().is_empty() {
                    return Err(bad("Нужно устройство вывода (имя или описание)"));
                }
                Ok(Command::StreamOutput(idx, sink))
            }
            Some(s) => Err(bad(format!("Неизвестная команда звука: {s}"))),
        },
        "brightness" => no_more(2, Command::Brightness(level(arg(1))?)),
        "night" | "dark" | "dnd" => {
            let mode = match head { "night" => Mode::Night, "dark" => Mode::Dark, _ => Mode::Dnd };
            no_more(2, Command::Mode(mode, switch(arg(1), Switch::Toggle)?))
        }
        "display" => no_more(1, Command::Display),
        "media" => match arg(1) {
            None | Some("status") => no_more(2, Command::MediaStatus),
            Some("play-pause" | "play" | "pause") => no_more(2, Command::PlayPause),
            Some("next") => no_more(2, Command::Next),
            Some("previous" | "prev") => no_more(2, Command::Previous),
            Some(s) => Err(bad(format!("Неизвестная команда медиа: {s}"))),
        },
        "screenshot" => match arg(1) {
            None => no_more(1, Command::Screenshot("screen".into())),
            Some(t @ ("screen" | "window")) => no_more(2, Command::Screenshot(t.into())),
            Some(s) => Err(bad(format!("Снимок: screen или window, а не {s}"))),
        },
        "larvis" => match arg(1) {
            None | Some("status") => no_more(2, Command::LarvisStatus),
            s => no_more(2, Command::Larvis(switch(s, Switch::Toggle)?)),
        },
        "lock" => no_more(1, Command::Lock),
        "suspend" => no_more(1, Command::Suspend),
        s => Err(bad(format!("Неизвестная команда: {s}. featherctl help — список команд"))),
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "вкл" } else { "выкл" }
}

fn print_json<T: serde::Serialize>(value: &T) {
    println!("{}", serde_json::to_string_pretty(value).unwrap_or_default());
}

fn run(proxy: &Shell1ProxyBlocking, command: Command, json: bool) -> Result<(), Error> {
    match command {
        Command::Help => println!("{HELP}"),
        Command::Version => {
            let (v, c) = (proxy.version()?, proxy.compositor_version()?);
            if json { print_json(&serde_json::json!({"api": v, "compositor": c})) } else { println!("API {v}, Пёрышко {c}") }
        }
        Command::Watch => {
            // One stream keeps the signals in the order they were sent
            for m in proxy.inner().receive_all_signals()? {
                let header = m.header();
                let body = m.body();
                match header.member().map(|m| m.as_str()) {
                    Some("WindowOpened") => {
                        if let Ok((id, app_id, title)) = body.deserialize::<(u64, String, String)>() {
                            println!("opened {id} {app_id} {title:?}");
                        }
                    }
                    Some("WindowClosed") => {
                        if let Ok(id) = body.deserialize::<u64>() {
                            println!("closed {id}");
                        }
                    }
                    Some("FocusChanged") => {
                        if let Ok(id) = body.deserialize::<u64>() {
                            println!("focus {id}");
                        }
                    }
                    Some("SpaceChanged") => {
                        if let Ok((number, count, larvis)) = body.deserialize::<(u32, u32, bool)>() {
                            println!("space {number}/{count}{}", if larvis { " larvis" } else { "" });
                        }
                    }
                    _ => continue,
                }
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
        }
        Command::Windows => {
            let list = proxy.list_windows()?;
            if json {
                print_json(&list);
            } else {
                for w in list {
                    let mut flags = Vec::new();
                    if w.focused { flags.push("фокус") }
                    if w.minimized { flags.push("свёрнуто") }
                    if w.maximized { flags.push("развёрнуто") }
                    if w.fullscreen { flags.push("полный экран") }
                    let app = if w.app_name.is_empty() { &w.app_id } else { &w.app_name };
                    println!("{:>4}  стол {}  {:<20} {}{}", w.id, w.space, app, w.title, if flags.is_empty() { String::new() } else { format!("  [{}]", flags.join(", ")) });
                }
            }
        }
        Command::Focus(id) => proxy.focus_window(id)?,
        Command::Close(id) => proxy.close_window(id)?,
        Command::Minimize(id) => proxy.minimize_window(id)?,
        Command::Maximize(id, s) | Command::Fullscreen(id, s) => {
            let full = matches!(command, Command::Fullscreen(..));
            let current = || -> Result<bool, Error> {
                let list = proxy.list_windows()?;
                let w = list.iter().find(|w| if id == 0 { w.focused } else { w.id == id });
                let w = w.ok_or_else(|| Error::NotFound(format!("Нет окна {id}")))?;
                Ok(if full { w.fullscreen } else { w.maximized })
            };
            let on = if s == Switch::Toggle { !current()? } else { s.resolve(false) };
            if full { proxy.fullscreen_window(id, on)? } else { proxy.maximize_window(id, on)? }
        }
        Command::MoveWindow(id, space) => proxy.move_window_to_space(id, space)?,
        Command::WindowPip(id, action) => match action {
            PipAction::Status => {
                let (pip, ghost) = proxy.get_window_pip(id)?;
                if json {
                    print_json(&serde_json::json!({"pip": pip, "ghost": ghost}));
                } else {
                    println!("pip: {}, сквозь курсор: {}", on_off(pip), on_off(ghost));
                }
            }
            PipAction::On => proxy.set_window_pip(id, true, false)?,
            PipAction::Off => proxy.set_window_pip(id, false, false)?,
            PipAction::Toggle => {
                let (pip, _) = proxy.get_window_pip(id)?;
                proxy.set_window_pip(id, !pip, false)?;
            }
            PipAction::GhostToggle => {
                let (pip, ghost) = proxy.get_window_pip(id)?;
                proxy.set_window_pip(id, true, if pip { !ghost } else { true })?;
            }
        },
        Command::Spaces => {
            let list = proxy.list_spaces()?;
            if json {
                print_json(&list);
            } else {
                for s in list {
                    println!("{}{}  окон: {}", s.number, if s.current { " *" } else { "  " }, s.windows);
                }
            }
        }
        Command::SwitchSpace(n) => proxy.switch_space(n)?,
        Command::GetSpaceMode(n) => {
            let mode = proxy.get_space_mode(n)?;
            if json {
                print_json(&serde_json::json!({"space": n, "mode": mode}));
            } else {
                println!("{mode}");
            }
        }
        Command::SetSpaceMode(n, mode) => proxy.set_space_mode(n, &mode)?,
        Command::Mission(on) => proxy.show_mission_control(on)?,
        Command::Launchpad(on) => proxy.show_launchpad(on)?,
        Command::MenuHud(s) => {
            let show = if s == Switch::Toggle {
                !proxy.get_menu_hud()?
            } else {
                s.resolve(false)
            };
            proxy.show_menu_hud(show)?;
        }
        Command::ShelfStatus => {
            let s = proxy.get_drop_shelf()?;
            if json {
                print_json(&s);
            } else {
                println!(
                    "полка: {}, элементов: {}",
                    if s.open { "открыта" } else { "скрыта" },
                    s.items.len()
                );
                for item in s.items {
                    if item.path.is_empty() {
                        println!("  {:>3}  [{:<6}] {} — {}", item.id, item.kind, item.title, item.subtitle);
                    } else {
                        println!("  {:>3}  [{:<6}] {} ({})", item.id, item.kind, item.title, item.path);
                    }
                }
            }
        }
        Command::ShelfShow(s) => {
            let show = if s == Switch::Toggle {
                !proxy.get_drop_shelf()?.open
            } else {
                s.resolve(false)
            };
            proxy.show_drop_shelf(show)?;
        }
        Command::ShelfClear => proxy.clear_drop_shelf()?,
        Command::ShelfAdd(paths) => {
            let added = proxy.add_to_drop_shelf(paths)?;
            if json {
                print_json(&serde_json::json!({"added": added}));
            } else {
                println!("{added}");
            }
        }
        Command::Apps => {
            let list = proxy.list_apps()?;
            if json {
                print_json(&list);
            } else {
                for a in list {
                    println!("{:<40} {}{}", a.id, a.name, if a.running { "  [открыто]" } else { "" });
                }
            }
        }
        Command::Launch(id) => proxy.launch_app(&id)?,
        Command::Activate(id) => proxy.activate_app(&id)?,
        Command::Volume(Level::Show) | Command::MediaStatus | Command::Display => {
            let show = |value: serde_json::Value, text: String| if json { print_json(&value) } else { println!("{text}") };
            match command {
                Command::Volume(_) => {
                    let a = proxy.get_audio()?;
                    let mut text = if a.available {
                        format!("{} %{}  ({})", a.volume, if a.muted { ", без звука" } else { "" }, a.output)
                    } else {
                        "Нет устройства вывода звука".into()
                    };
                    if a.mic_available {
                        text += &format!("\nмикрофон: {} %{}  ({})", a.mic_volume, if a.mic_muted { ", выключен" } else { "" }, a.input);
                    } else {
                        text += "\nмикрофона нет";
                    }
                    show(serde_json::to_value(&a).unwrap_or_default(), text);
                }
                Command::Display => {
                    let d = proxy.get_display()?;
                    let b = if d.brightness < 0 { "недоступна".to_string() } else { format!("{} %", d.brightness) };
                    show(
                        serde_json::to_value(&d).unwrap_or_default(),
                        format!("яркость: {b}\nночной режим: {}\nтёмная тема: {}\nне беспокоить: {}", on_off(d.night_shift), on_off(d.dark_mode), on_off(d.do_not_disturb)),
                    );
                }
                _ => {
                    let m = proxy.get_media()?;
                    let text = if m.available { format!("{} — {} — {} ({})", m.player, m.artist, m.title, if m.playing { "играет" } else { "пауза" }) } else { "Плеер не найден".into() };
                    show(serde_json::to_value(&m).unwrap_or_default(), text);
                }
            }
        }
        Command::Volume(Level::Set(v)) => println!("{}", proxy.set_volume(v)?),
        Command::Volume(Level::Change(d)) => println!("{}", proxy.change_volume(d)?),
        Command::Mute(s) => {
            let on = if s == Switch::Toggle { !proxy.get_audio()?.muted } else { s.resolve(false) };
            proxy.set_mute(on)?
        }
        Command::MicMute(s) => {
            let on = if s == Switch::Toggle { !proxy.get_audio()?.mic_muted } else { s.resolve(false) };
            proxy.set_mic_mute(on)?
        }
        Command::AudioStreams => {
            let streams = proxy.list_audio_streams()?;
            if json {
                print_json(&streams);
            } else if streams.is_empty() {
                println!("Нет активных звуковых потоков");
            } else {
                for st in streams {
                    let out = if st.sink_label.is_empty() { &st.sink } else { &st.sink_label };
                    let sub = if st.subtitle.is_empty() { String::new() } else { format!(" — {}", st.subtitle) };
                    println!(
                        "{:>4}  {:>3} %{}  [{out}]  {}{}",
                        st.index,
                        st.volume,
                        if st.muted { " [без звука]" } else { "" },
                        st.name,
                        sub
                    );
                }
            }
        }
        Command::StreamVolume(index, vol) => println!("{}", proxy.set_stream_volume(index, vol)?),
        Command::StreamMute(index, s) => {
            let muted = if s == Switch::Toggle {
                let streams = proxy.list_audio_streams()?;
                let cur = streams
                    .iter()
                    .find(|st| st.index == index)
                    .ok_or_else(|| Error::NotFound(format!("Нет звукового потока #{index}")))?;
                !cur.muted
            } else {
                s.resolve(false)
            };
            proxy.set_stream_mute(index, muted)?;
        }
        Command::StreamOutput(index, sink) => proxy.move_stream_output(index, &sink)?,
        Command::Brightness(Level::Show) => {
            let b = proxy.get_display()?.brightness;
            if b < 0 { return Err(Error::Unavailable("Яркость этого экрана не управляется".into())) }
            println!("{b}");
        }
        Command::Brightness(Level::Set(v)) => println!("{}", proxy.set_brightness(v)?),
        Command::Brightness(Level::Change(d)) => println!("{}", proxy.change_brightness(d)?),
        Command::Mode(mode, s) => {
            let current = || -> Result<bool, Error> {
                let d = proxy.get_display()?;
                Ok(match mode { Mode::Night => d.night_shift, Mode::Dark => d.dark_mode, Mode::Dnd => d.do_not_disturb })
            };
            let on = if s == Switch::Toggle { !current()? } else { s.resolve(false) };
            match mode {
                Mode::Night => proxy.set_night_shift(on)?,
                Mode::Dark => proxy.set_dark_mode(on)?,
                Mode::Dnd => proxy.set_do_not_disturb(on)?,
            }
        }
        Command::PlayPause => proxy.media_play_pause()?,
        Command::Next => proxy.media_next()?,
        Command::Previous => proxy.media_previous()?,
        Command::Screenshot(target) => println!("{}", proxy.screenshot(&target)?),
        Command::LarvisStatus => {
            let (running, shown) = proxy.get_larvis_desktop()?;
            if json {
                print_json(&serde_json::json!({"running": running, "shown": shown}))
            } else {
                println!("{}", if !running { "не запущен" } else if shown { "показан" } else { "скрыт" });
            }
        }
        Command::Larvis(s) => {
            let shown = if s == Switch::Toggle { !proxy.get_larvis_desktop()?.1 } else { s.resolve(false) };
            proxy.show_larvis_desktop(shown)?
        }
        Command::Lock => proxy.lock_screen()?,
        Command::Suspend => proxy.suspend()?,
    }
    Ok(())
}

/// Сообщение ошибки без имени D-Bus, если сервер его дал
fn message(e: &Error) -> String {
    use zbus::DBusError;
    match e {
        Error::ZBus(zbus::Error::MethodError(name, Some(text), _)) => format!("{text} ({name})"),
        Error::ZBus(zbus::Error::FDO(f)) if matches!(**f, zbus::fdo::Error::ServiceUnknown(_)) => {
            format!("Пёрышко не отвечает: на шине нет {}", feather_api::bus_name())
        }
        Error::ZBus(e) => e.to_string(),
        other => other.description().unwrap_or("ошибка").to_string(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let args: Vec<&str> = args.iter().map(String::as_str).filter(|a| *a != "--json").collect();
    let command = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("featherctl: {e}");
            std::process::exit(2);
        }
    };
    if command == Command::Help {
        println!("{HELP}");
        return;
    }
    let result = connect().and_then(|proxy| run(&proxy, command, json));
    if let Err(e) = result {
        eprintln!("featherctl: {}", message(&e));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Command, String> {
        parse(&s.split_whitespace().collect::<Vec<_>>())
    }

    #[test]
    fn windows_and_spaces() {
        assert_eq!(p("window list"), Ok(Command::Windows));
        assert_eq!(p("window"), Ok(Command::Windows));
        assert_eq!(p("window focus 12"), Ok(Command::Focus(12)));
        assert_eq!(p("window close active"), Ok(Command::Close(0)));
        assert_eq!(p("window maximize 3"), Ok(Command::Maximize(3, Switch::Toggle)));
        assert_eq!(p("window fullscreen active off"), Ok(Command::Fullscreen(0, Switch::Off)));
        assert_eq!(p("window move 7 2"), Ok(Command::MoveWindow(7, 2)));
        assert!(p("window move 7").is_err());
        assert!(p("window focus").is_err());
        assert!(p("window focus x").is_err());
        assert!(p("window focus 1 2").is_err(), "extra arguments are refused");
        assert_eq!(p("space"), Ok(Command::Spaces));
        assert_eq!(p("space 2"), Ok(Command::SwitchSpace(2)));
        assert_eq!(p("space-mode"), Ok(Command::GetSpaceMode(0)));
        assert_eq!(p("space-mode 2"), Ok(Command::GetSpaceMode(2)));
        assert_eq!(p("space-mode tile"), Ok(Command::SetSpaceMode(0, "tile".into())));
        assert_eq!(p("space-mode tape 3"), Ok(Command::SetSpaceMode(3, "tape".into())));
        assert_eq!(p("space mode next"), Ok(Command::SetSpaceMode(0, "next".into())));
        assert!(p("space-mode grid").is_err());
        assert!(p("space-mode tile 0").is_err());
        assert_eq!(p("mission"), Ok(Command::Mission(true)));
        assert_eq!(p("launchpad off"), Ok(Command::Launchpad(false)));
        assert_eq!(p("shelf"), Ok(Command::ShelfStatus));
        assert_eq!(p("shelf toggle"), Ok(Command::ShelfShow(Switch::Toggle)));
        assert_eq!(p("shelf open"), Ok(Command::ShelfShow(Switch::On)));
        assert_eq!(p("shelf close"), Ok(Command::ShelfShow(Switch::Off)));
        assert_eq!(p("shelf clear"), Ok(Command::ShelfClear));
        assert_eq!(p("shelf add /tmp/a.txt"), Ok(Command::ShelfAdd(vec!["/tmp/a.txt".into()])));
        assert!(p("shelf add").is_err());
    }

    #[test]
    fn levels_and_switches() {
        assert_eq!(p("volume"), Ok(Command::Volume(Level::Show)));
        assert_eq!(p("volume +5"), Ok(Command::Volume(Level::Change(5))));
        assert_eq!(p("volume -10"), Ok(Command::Volume(Level::Change(-10))));
        assert_eq!(p("volume 40%"), Ok(Command::Volume(Level::Set(40))));
        assert_eq!(p("volume mute"), Ok(Command::Mute(Switch::On)));
        assert_eq!(p("volume toggle"), Ok(Command::Mute(Switch::Toggle)));
        assert_eq!(p("mic unmute"), Ok(Command::MicMute(Switch::Off)));
        assert_eq!(p("brightness -10"), Ok(Command::Brightness(Level::Change(-10))));
        assert!(p("volume loud").is_err());
        assert_eq!(p("night"), Ok(Command::Mode(Mode::Night, Switch::Toggle)));
        assert_eq!(p("dark on"), Ok(Command::Mode(Mode::Dark, Switch::On)));
        assert_eq!(p("dnd off"), Ok(Command::Mode(Mode::Dnd, Switch::Off)));
        assert_eq!(p("larvis"), Ok(Command::LarvisStatus));
        assert_eq!(p("larvis toggle"), Ok(Command::Larvis(Switch::Toggle)));
        assert_eq!(p("larvis off"), Ok(Command::Larvis(Switch::Off)));
        assert!(p("larvis maybe").is_err());
        assert!(p("dnd maybe").is_err());
        assert!(Switch::Toggle.resolve(false) && !Switch::Toggle.resolve(true) && !Switch::Off.resolve(true));
    }

    #[test]
    fn apps_media_and_system() {
        assert_eq!(p("app list"), Ok(Command::Apps));
        assert_eq!(p("app launch firefox"), Ok(Command::Launch("firefox".into())));
        assert_eq!(p("app open org.gnome.Nautilus"), Ok(Command::Activate("org.gnome.Nautilus".into())));
        assert!(p("app launch").is_err());
        assert_eq!(p("media"), Ok(Command::MediaStatus));
        assert_eq!(p("media prev"), Ok(Command::Previous));
        assert_eq!(p("screenshot"), Ok(Command::Screenshot("screen".into())));
        assert_eq!(p("screenshot window"), Ok(Command::Screenshot("window".into())));
        assert!(p("screenshot area").is_err());
        assert_eq!(p("suspend"), Ok(Command::Suspend));
        assert_eq!(p(""), Ok(Command::Help));
        assert!(p("reboot").is_err());
    }

    #[test]
    fn pip_hud_and_audio_mixer() {
        assert_eq!(p("window pip"), Ok(Command::WindowPip(0, PipAction::Toggle)));
        assert_eq!(p("window pip ghost"), Ok(Command::WindowPip(0, PipAction::GhostToggle)));
        assert_eq!(p("window pip status"), Ok(Command::WindowPip(0, PipAction::Status)));
        assert_eq!(p("window pip 42 on"), Ok(Command::WindowPip(42, PipAction::On)));
        assert_eq!(p("window pip active off"), Ok(Command::WindowPip(0, PipAction::Off)));
        assert_eq!(p("window pip focused ghost"), Ok(Command::WindowPip(0, PipAction::GhostToggle)));
        assert!(p("window pip 42 banana").is_err());

        assert_eq!(p("menu-hud"), Ok(Command::MenuHud(Switch::Toggle)));
        assert_eq!(p("menu-hud open"), Ok(Command::MenuHud(Switch::On)));
        assert_eq!(p("hud close"), Ok(Command::MenuHud(Switch::Off)));

        assert_eq!(p("audio"), Ok(Command::AudioStreams));
        assert_eq!(p("audio streams"), Ok(Command::AudioStreams));
        assert_eq!(p("audio app-volume 57 75%"), Ok(Command::StreamVolume(57, 75)));
        assert!(p("audio app-volume 57 120").is_err());
        assert_eq!(p("audio app-mute 57"), Ok(Command::StreamMute(57, Switch::Toggle)));
        assert_eq!(p("audio app-mute 57 mute"), Ok(Command::StreamMute(57, Switch::On)));
        assert_eq!(p("audio app-mute 57 off"), Ok(Command::StreamMute(57, Switch::Off)));
        assert_eq!(
            p("audio app-output 57 USB ЦАП Наушники"),
            Ok(Command::StreamOutput(57, "USB ЦАП Наушники".into()))
        );
        assert!(p("audio app-output 57").is_err());
    }
}
