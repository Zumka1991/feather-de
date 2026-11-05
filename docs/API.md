# API рабочего стола: `org.feather.Shell1`

Внешние программы — в первую очередь голосовой ассистент Ларвис, а также скрипты и горячие
клавиши — управляют Пёрышком через сессионную шину D-Bus.

| | |
|---|---|
| Имя на шине | `org.feather.Shell1` (другое имя — переменная `FEATHER_API_NAME`) |
| Путь объекта | `/org/feather/Shell1` |
| Интерфейс | `org.feather.Shell1` |
| Версия интерфейса | свойство `Version` = `1` |
| Клиент на Rust | крейт [`crates/feather-api`](../crates/feather-api) (`feather_api::connect()`) |
| Утилита | `featherctl` (`featherctl help`) |

## Устройство

- Сервер — модуль композитора [`src/api/`](../src/api). D-Bus обслуживает собственный поток
  `feather-api`. Метод превращается в запрос `Call` и уходит в цикл событий через ограниченный
  `calloop::channel` (32 запроса). Цикл выполняет запрос теми же путями, что клики и клавиши
  оболочки, и сразу отвечает через одноразовый канал. В цикле нет ни D-Bus, ни I/O, ни
  ожиданий. Снимок экрана отвечает путём из своего потока кодирования.
- Если очередь переполнена, метод возвращает `Failed` («Пёрышко занято»). Цикл никогда не ждёт
  поток API.
- Сигналы окон собираются в `refresh_focus()`: через него проходят все открытия, закрытия и
  смены фокуса. В простое поток спит на канале, 0 % CPU.
- Своё имя сеанс Пёрышка (DRM или `--standalone`) забирает у зависшего вложенного запуска.
  Вложенный (`--windowed`) запуск получает имя, только если оно свободно. Оба разрешают
  последующую замену. `FEATHER_API=0` выключает сервис.

## Соглашения

- **Окна** обозначаются числом `id` из `ListWindows`. `id = 0` — активное окно (верхнее видимое
  на текущем столе).
- **Рабочие столы** нумеруются с **1**.
- **Проценты** громкости и яркости — `0…100`. Большие значения в `Set*` срезаются до 100.
  Громкость выше 100 % (её умеет PipeWire) `ChangeVolume` не увеличивает.
- Пока экран заблокирован, работают только чтение (`List*`, `Get*`), звук, медиа, `LockScreen`
  и `Suspend`. Остальные методы возвращают `Unavailable` («Экран заблокирован»).

## Опасные методы

Могут потерять несохранённые данные пользователя. Клиент (Ларвис) обязан спросить
подтверждение до вызова. Список доступен как `feather_api::DANGEROUS`.

| Метод | Почему опасен |
|---|---|
| `CloseWindow` | программа закрывает окно; без диалога сохранения данные теряются |
| `Suspend` | сон всей машины; в своём сеансе блокирует экран и засыпает |

Выход из сеанса, перезагрузка и выключение через API не доступны.

## Ошибки

Имя ошибки — `org.feather.Shell1.Error.<Вид>`, текст — по-русски, для показа пользователю.

| Ошибка | Когда |
|---|---|
| `NotFound` | нет окна, приложения, рабочего стола или плеера |
| `InvalidArgs` | неверный аргумент: стол 0, неизвестная цель снимка, пустой ID приложения |
| `Unavailable` | служба недоступна: нет звука, экран без управления яркостью, экран заблокирован |
| `Failed` | запрос не выполнен: очередь занята, снимок не сохранился |

## Типы

| Тип | Подпись D-Bus | Поля |
|---|---|---|
| `WindowInfo` | `(tsssubbbb)` | `id`, `app_id`, `app_name` (из `.desktop`, пусто для неизвестных), `title`, `space` (с 1), `focused`, `minimized`, `maximized`, `fullscreen` |
| `SpaceInfo` | `(ubu)` | `number` (с 1), `current`, `windows` (открытых окон, включая свёрнутые) |
| `AppInfo` | `(sssb)` | `id`, `name`, `description`, `running` |
| `AudioInfo` | `(bsubbsub)` | `available`, `output`, `volume`, `muted`, `mic_available`, `input`, `mic_volume`, `mic_muted` |
| `DisplayInfo` | `(ibbb)` | `brightness` (-1 — не управляется), `night_shift`, `dark_mode`, `do_not_disturb` |
| `MediaInfo` | `(bsssb)` | `available`, `player`, `title`, `artist`, `playing` |

В крейте `feather-api` это структуры с `serde`. `featherctl --json` печатает их в JSON.

## Методы

### Окна

| Метод | Аргументы → ответ | Что делает |
|---|---|---|
| `ListWindows` | → `a(tsssubbbb)` | открытые окна, сверху вниз по стопке |
| `FocusWindow` | `t id` | показывает окно: переходит на его стол, разворачивает из Dock |
| `CloseWindow` ⚠️ | `t id` | просит программу закрыть окно (как красная кнопка) |
| `MinimizeWindow` | `t id` | сворачивает в Dock |
| `MaximizeWindow` | `t id, b maximized` | разворачивает на экран или возвращает прежний размер |
| `FullscreenWindow` | `t id, b fullscreen` | полноэкранный режим |
| `MoveWindowToSpace` | `t id, u space` | переносит окно на стол `space`; номер «последний + 1» создаёт новый стол |

### Рабочие столы

| Метод | Аргументы → ответ | Что делает |
|---|---|---|
| `ListSpaces` | → `a(ubu)` | столы по порядку |
| `SwitchSpace` | `u number` | переходит на стол |
| `ShowMissionControl` | `b show` | открывает или закрывает Mission Control |
| `ShowLaunchpad` | `b show` | открывает или закрывает Launchpad |

### Стол Ларвиса

Ларвис выполняет задачи с окнами на своём столе: это вложенное Пёрышко
(`feather-de --larvis-desktop`) со своими окнами, клавиатурой и указателем. В основном
сеансе его окно с `app_id` `org.feather.LarvisDesktop` — не задача пользователя, а отдельный
стол после столов пользователя. Его окна нет в Dock, Alt+Tab, `ListWindows` и сигналах окон,
а `ListSpaces` перечисляет только столы пользователя. Пока стол запущен, в меню-баре рядом с
треем стоит значок Ларвиса (экран с точкой).

- Щелчок по значку или Cmd + U показывает стол Ларвиса. Окна пользователя скрываются, как
  при смене стола. Меню-бар остаётся на месте, вложенный стол свой меню-бар не рисует.
  Значок на столе Ларвиса залит. Повторный щелчок, Cmd + U, Cmd + W или Cmd + M возвращают
  на стол, где был пользователь.
- Ctrl + Alt + → с последнего стола пользователя ведёт на стол Ларвиса, Ctrl + Alt + ← с него —
  на последний стол пользователя. Пока стол Ларвиса запущен, новый стол добавляют через «+»
  в Mission Control или Ctrl + Alt + Shift + → (с окном).
- В Mission Control после столов пользователя идёт плитка «Ларвис» с живой миниатюрой; щелчок
  по ней показывает стол Ларвиса. Mission Control открывается и поверх стола Ларвиса.
- На стол пользователя возвращают и переход на его стол, Dock, Alt+Tab и новое окно
  пользователя. Cmd + Shift + U на открытом столе Ларвиса передаёт пользователю ручное управление.

| Метод | Аргументы → ответ | Что делает |
|---|---|---|
| `GetLarvisDesktop` | → `b running, b shown` | запущен ли стол Ларвиса и показан ли он сейчас |
| `ShowLarvisDesktop` | `b show` | показывает стол Ларвиса или возвращает на стол пользователя, как значок в меню-баре. Если стол не запущен, `true` возвращает `Unavailable`. На самом столе Ларвиса метод недоступен |
| `StopLarvisDesktop` | | завершает вложенный стол Ларвиса. Вызывается на его собственной шине; в основном сеансе возвращает `Unavailable` |

Свойство `LarvisDesktopSupported` (`b`) говорит Ларвису, что этот Feather умеет запускать его стол.

### Приложения

| Метод | Аргументы → ответ | Что делает |
|---|---|---|
| `ListApps` | → `a(sssb)` | приложения Launchpad (`.desktop` и встроенные), по алфавиту |
| `LaunchApp` | `s app_id` | запускает новый экземпляр |
| `ActivateApp` | `s app_id` | выводит окна приложения вперёд, а если их нет — запускает (как щелчок в Dock) |

`app_id` ищется по ID из `ListApps`, затем по Wayland app_id/`StartupWMClass`/имени исполняемого
файла, затем по названию без учёта регистра (`firefox`, `org.gnome.Nautilus`, «Терминал»).

### Звук

Громкость меняется с тем же HUD, что у мультимедийных клавиш. Команду выполняет поток звука
(`pactl`). `SetVolume`/`ChangeVolume` отвечают громкостью, которая будет установлена, а
`GetAudio` показывает её после того, как поток пришлёт новое состояние (доли секунды). Несколько
`ChangeVolume` подряд без паузы могут считаться от одного и того же старого значения.

| Метод | Аргументы → ответ | Что делает |
|---|---|---|
| `GetAudio` | → `(bsubbsub)` | устройство вывода и микрофон по умолчанию |
| `SetVolume` | `u percent` → `u` | ставит громкость; значение выше 0 включает выключенный звук |
| `ChangeVolume` | `i delta` → `u` | меняет на `delta` процентов; рост включает звук |
| `SetMute` | `b muted` | выключает или включает звук |
| `SetMicMute` | `b muted` | выключает или включает микрофон |

### Экран и режимы

| Метод | Аргументы → ответ | Что делает |
|---|---|---|
| `GetDisplay` | → `(ibbb)` | яркость и режимы |
| `SetBrightness` | `u percent` → `u` | яркость (logind или DDC/CI); во вложенном запуске — `Unavailable` |
| `ChangeBrightness` | `i delta` → `u` | меняет яркость на `delta` процентов |
| `SetNightShift` | `b on` | ночной режим, как переключатель в Пункте управления |
| `SetDarkMode` | `b on` | тёмная или светлая тема |
| `SetDoNotDisturb` | `b on` | «Не беспокоить» |
| `GetDockEdge` | → `s` | текущее положение Dock в запущенном сеансе |
| `SetDockEdge` | `s edge` | применяет `left`, `bottom` или `right` через тот же путь, что кнопка в Системных настройках |

### Медиа (MPRIS2)

Команды получает текущий плеер — тот, что показан в Пункте управления.

| Метод | Аргументы → ответ | Что делает |
|---|---|---|
| `GetMedia` | → `(bsssb)` | текущий плеер и трек |
| `MediaPlayPause` | | воспроизведение/пауза |
| `MediaNext` | | следующий трек |
| `MediaPrevious` | | предыдущий трек |

Без плеера команды возвращают `NotFound`.

### Система

| Метод | Аргументы → ответ | Что делает |
|---|---|---|
| `Screenshot` | `s target` → `s path` | снимок `screen` (весь экран) или `window` (активное окно). Как клавиша Print: файл в папке снимков (Системные настройки → Снимки экрана), копия в буфере обмена, уведомление. Возвращает путь к файлу |
| `LockScreen` | | блокирует экран |
| `Suspend` ⚠️ | | блокирует экран и усыпляет машину. Во вложенном запуске только блокирует |

### Управление интерфейсом приложений

Методы доступны с Feather DE 0.11.0. Чтобы включить автоматизацию при следующем входе
через `feather-session`, создайте файл `enable-automation` в каталоге настроек Feather.
При ручном запуске композитора задайте `FEATHER_AUTOMATION=1`. Значение `0` выключает
автоматизацию, даже если файл существует. Доступ к методам получают процессы
пользователя на сессионной шине. Текущий статус показывает свойство `AutomationEnabled`.
Поддерживаются активные окна Wayland и XWayland.

```bash
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/feather-de"
touch "${XDG_CONFIG_HOME:-$HOME/.config}/feather-de/enable-automation"
```

| Метод | Аргументы → ответ | Действие |
|---|---|---|
| `ObserveWindow` | `t id` → `tuuay token, width, height, png` | PNG выбранного окна с его всплывающими элементами, без других окон, курсора и системной оболочки |
| `WindowClick` | `tuuuu token, x, y, button, count` | Левая, правая или средняя кнопка (1–3), один или два клика |
| `WindowScroll` | `tuui token, x, y, steps` | Вертикальная прокрутка на 1–12 шагов, положительное число вниз |
| `WindowKey` | `tau token, keys` | Сочетание из 1–4 кодов evdev, сначала модификаторы |
| `WindowPaste` | `tsau token, text, keys` | Заменить буфер обмена текстом UTF-8 до 64 КиБ и нажать сочетание вставки |

Для `ObserveWindow` нужен конкретный ID из `ListWindows`. Сначала вызовите `FocusWindow`.
Токен действует до 90 секунд и разрешает одну операцию ввода. Нужен новый снимок после
каждой операции. Координаты `x` и `y` идут от 0 до 999 по ширине и высоте полученного
изображения. Ввод проверяет, что окно всё ещё активно, его геометрия совпадает со снимком,
а точка принадлежит его поверхности. Действия пользователя сбрасывают токен. На экране
блокировки и в системном диалоге аутентификации автоматизация недоступна.

### Свойства

| Свойство | Тип | Значение |
|---|---|---|
| `Version` | `u` | версия интерфейса, сейчас `1` |
| `CompositorVersion` | `s` | версия Пёрышка, например `0.9.1` |
| `AutomationEnabled` | `b` | включено ли управление интерфейсом приложений |
| `LarvisDesktopSupported` | `b` | этот Feather умеет запускать стол Ларвиса |

### Сигналы

| Сигнал | Аргументы | Когда |
|---|---|---|
| `WindowOpened` | `t id, s app_id, s title` | окно появилось на экране (первый буфер) |
| `WindowClosed` | `t id` | окно закрыто или скрыто программой |
| `FocusChanged` | `t id` | сменилось активное окно; `0` — фокуса нет (рабочий стол, блокировка) |
| `SpaceChanged` | `u number, u count, b larvis` | сменился стол, число столов или показ стола Ларвиса. `number` — стол пользователя с 1 (на столе Ларвиса — тот, что под ним), `count` — число столов пользователя, `larvis` — показан ли стол Ларвиса |

## featherctl

```
featherctl [--json] КОМАНДА
```

| Команда | Метод |
|---|---|
| `featherctl window list` | `ListWindows` |
| `featherctl window focus 12` | `FocusWindow(12)` |
| `featherctl window close active` | `CloseWindow(0)` ⚠️ |
| `featherctl window minimize active` | `MinimizeWindow(0)` |
| `featherctl window maximize 12 [on\|off\|toggle]` | `MaximizeWindow` |
| `featherctl window fullscreen active toggle` | `FullscreenWindow` |
| `featherctl window move 12 2` | `MoveWindowToSpace(12, 2)` |
| `featherctl space` / `featherctl space 2` | `ListSpaces` / `SwitchSpace(2)` |
| `featherctl mission [on\|off]`, `featherctl launchpad [on\|off]` | `ShowMissionControl`, `ShowLaunchpad` |
| `featherctl larvis [status]` / `featherctl larvis on\|off\|toggle` | `GetLarvisDesktop` / `ShowLarvisDesktop` |
| `featherctl app list` / `app launch firefox` / `app open firefox` | `ListApps` / `LaunchApp` / `ActivateApp` |
| `featherctl volume` / `volume 40` / `volume +5` / `volume -5` | `GetAudio` / `SetVolume` / `ChangeVolume` |
| `featherctl volume mute\|unmute\|toggle`, `featherctl mic mute\|unmute\|toggle` | `SetMute`, `SetMicMute` |
| `featherctl brightness [N\|+N\|-N]` | `GetDisplay` / `SetBrightness` / `ChangeBrightness` |
| `featherctl night\|dark\|dnd [on\|off\|toggle]` | `SetNightShift`, `SetDarkMode`, `SetDoNotDisturb` |
| `featherctl display` | `GetDisplay` |
| `featherctl media [status\|play-pause\|next\|previous]` | `GetMedia`, `Media*` |
| `featherctl screenshot [screen\|window]` | `Screenshot`, печатает путь |
| `featherctl lock`, `featherctl suspend` | `LockScreen`, `Suspend` ⚠️ |
| `featherctl version` | свойства `Version`, `CompositorVersion` |
| `featherctl watch` | печатает сигналы окон и столов, пока не прервать |

`toggle` читает текущее состояние и вызывает метод с противоположным значением. Коды выхода:
`0` — успех, `1` — ошибка API (текст в stderr), `2` — неверные аргументы.

## busctl

```bash
busctl --user call org.feather.Shell1 /org/feather/Shell1 org.feather.Shell1 ListWindows
busctl --user call org.feather.Shell1 /org/feather/Shell1 org.feather.Shell1 FocusWindow t 0
busctl --user call org.feather.Shell1 /org/feather/Shell1 org.feather.Shell1 SwitchSpace u 2
busctl --user call org.feather.Shell1 /org/feather/Shell1 org.feather.Shell1 ChangeVolume i -10
busctl --user call org.feather.Shell1 /org/feather/Shell1 org.feather.Shell1 SetDarkMode b true
busctl --user call org.feather.Shell1 /org/feather/Shell1 org.feather.Shell1 LaunchApp s firefox
busctl --user call org.feather.Shell1 /org/feather/Shell1 org.feather.Shell1 Screenshot s window
busctl --user get-property org.feather.Shell1 /org/feather/Shell1 org.feather.Shell1 Version
busctl --user introspect org.feather.Shell1 /org/feather/Shell1
busctl --user monitor org.feather.Shell1
```

## Python (Ларвис)

```python
from dbus_next.aio import MessageBus

bus = await MessageBus().connect()
node = await bus.introspect("org.feather.Shell1", "/org/feather/Shell1")
shell = bus.get_proxy_object("org.feather.Shell1", "/org/feather/Shell1", node) \
           .get_interface("org.feather.Shell1")
windows = await shell.call_list_windows()       # [[id, app_id, app_name, title, space, …], …]
await shell.call_change_volume(-10)
shell.on_window_opened(lambda id, app_id, title: print("открыто", title))
```

## Проверка

- `cargo test -p feather-api` — подписи типов, имена ошибок и разбор команд `featherctl`.
- `cargo test --release api::` — проверка аргументов, шаги громкости, запреты под блокировкой.
- `dbus-run-session -- cargo test --release api:: -- --include-ignored` — каждый метод на
  настоящей шине, с поддельным циклом событий: аргументы → `Call`, типы ответов, ошибки,
  сигналы и то, что второй экземпляр не отбирает имя.
- Живая проверка — на вложенном экземпляре со своей шиной:

```bash
dbus-run-session -- sh -c 'FEATHER_DEBUG_INPUT_ONLY=1 XDG_CONFIG_HOME=/tmp/fapi-cfg \
  target/release/feather-de --windowed & sleep 2; target/release/featherctl window list'
```
