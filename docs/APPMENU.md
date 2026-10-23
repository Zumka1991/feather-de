# Глобальное меню Qt/KDE, 2026-10-23

Реализован функциональный объём Qt/KDE задания 2. GTK-native `org.gtk.Menus` и
`org.gtk.Actions` пока не реализованы. Полная приёмка по бюджетам SPEC остаётся открытой.

## Поведение и архитектура

- `org_kde_kwin_appmenu_manager` v2 связывает D-Bus сервис/путь с `wl_surface` окна.
  Уничтожение старого appmenu-объекта не стирает адрес более нового объекта.
- Верхняя панель показывает собственное меню приложения, затем его экспортируемые
  меню. Если экспорта нет или сервис исчез — обычные «Окно / Справка».
- В меню есть вложенные панели с разворотом у правого края, галочки/radio, disabled,
  разделители, shortcuts, снятие mnemonic-маркеров, прокрутка длинных списков,
  стрелки/Enter/Esc. Рисование и hit testing используют общую геометрию.
- `GetLayout`, `AboutToShow`, `Event` и сигналы `LayoutUpdated`/`ItemsPropertiesUpdated`
  обслуживает отдельный worker с zbus/async-io. В calloop приходят готовые `Arc<MenuNode>`.
  Глубина ограничена 12, число узлов 4096; дубли ID отвергаются.
- Ограниченные очереди: 64 команды, 4 снимка, 1 уведомление смены фокуса. Главный поток
  не ждёт D-Bus или свободного места. Для Focus используется вытеснение старой команды.
  В простое нет опроса меню по таймеру. Одинаковый снимок не перерисовывает UI.
- Смена окна сразу очищает старое меню и меняет generation. Ответы и действия предыдущего
  поколения отвергаются; ожидающий RPC старому приложению отменяется отдельным событием.
  Зависшее `AboutToShow` не задерживает появление меню другого окна на timeout 2 с.
- Qt проверяет наличие `com.canonical.AppMenu.Registrar` ещё при старте. Сервис создаётся
  в DRM/standalone; `--autostart` ждёт готовности worker, сохраняя отзывчивость цикла.
  Во вложенном режиме имя по умолчанию не захватывается. Отладочное включение —
  `FEATHER_APPMENU_REGISTRAR=1` на отдельной шине. Существующий регистратор не заменяется.
  Запрос использует явный DoNotQueue без ReplaceExisting: обычный `zbus::request_name`
  по умолчанию включает замену, поэтому здесь не подходит.

Меню зависит от сборки приложения, а не только toolkit/названия. Установленный Chromium
экспортировал `/com/canonical/menu/1` через Wayland и показал собственное меню. Zenity/GTK
не экспортировал его и сохранил fallback. GTK-native остаётся отдельной задачей.
X11-привязка окон, иконки пунктов и управление меню по mnemonic/Alt не входят в этот объём.

## Проверки

`cargo build --release` должен завершаться без предупреждений.

```sh
dbus-run-session -- cargo test -- --include-ignored
```

10 тестов: прежние 4 анимационных, декодирование/лимиты/адреса, UI каскадов и действий,
disabled/hidden/смена окна, длинный список и интеграция с настоящей изолированной шиной.
Узкое окно проверяется на отсутствие выхода меню за границу и паники `clamp`.
Последняя проверяет AboutToShow/click, оба сигнала обновлений, старое поколение, выход
провайдера и загрузку следующего меню за <500 мс при зависшем предыдущем AboutToShow;
даже replaceable-владелец уже занятого Registrar остаётся владельцем имени.

Проверены собственные кадры вложенного композитора и `WAYLAND_DEBUG=1` клиентов:

| Клиент | Проверено |
|---|---|
| Qt6 `bench/appmenu.cpp` | File/Edit, каскад Nested, disabled, галочка, shortcut; автоматическое обновление Run test → Updated live, действие меняет текст на Action executed |
| Konsole `--separate --nofork` | Нативные меню; File/Clone Tab действительно создаёт вторую вкладку |
| Kate `--new --block` | Нативные меню; File/New создаёт документ Untitled рядом с исходным файлом |
| Dolphin `--new-window` | Нативные меню; File/New Tab создаёт вторую вкладку |
| Zenity / GTK | Клиент отображается с CSD, экспорт отсутствует, fallback оболочки сохранён |
| Chromium с отдельным user-data-dir | Клиент отображается, экспорт DBusMenu работает; `bench/anim.html` рисует и пишет FPS |

Действия запускались через отладочный hit testing нашей оболочки, без ввода в окна хоста.
Каждый сценарий использовал частную D-Bus шину и временную конфигурацию, Chromium — отдельный
профиль, KDE — отдельные экземпляры. Рабочая DRM-сессия пользователя не перезапускалась.

Заодно исправлен порядок работы EGL во вложенном backend: сначала native resize и
`MakeCurrent` поверхности, затем buffer age и SwapInterval(0); результат SwapInterval
проверяется. Импорт текстур может оставить контекст surfaceless. После debug readback
также восстанавливается framebuffer вывода перед submit. Это устраняет замеченные
`BAD_SURFACE`/`ContextLost` и блокирующий swap.

## Воспроизведение без ввода хосту

Из корня репозитория, при наличии Qt6 Widgets:

```sh
g++ -fPIC bench/appmenu.cpp -o /tmp/feather-menu-test $(pkg-config --cflags --libs Qt6Widgets)
dbus-run-session -- env XDG_CONFIG_HOME=/tmp/feather-menu-config \
  QT_NO_XDG_DESKTOP_PORTAL=1 FEATHER_APPMENU_REGISTRAR=1 \
  FEATHER_DEBUG_APPMENU='File/Updated live' FEATHER_DEBUG_APPMENU_ACTIVATE=1 \
  FEATHER_DEBUG_APPMENU_MS=4200 FEATHER_DEBUG_SCREENSHOT_MS=4700 \
  FEATHER_DEBUG_SCREENSHOT=/tmp/feather-menu.png \
  timeout 7 target/release/feather-de --windowed \
  --autostart 'QT_QPA_PLATFORM=wayland QT_QPA_PLATFORMTHEME=generic WAYLAND_DEBUG=1 /tmp/feather-menu-test 2>/tmp/feather-menu-client.log'
```

Ожидается `FEATHER_TEST_CLICKED` в логе и `Action executed` на собственном снимке.
Для каскада: `FEATHER_DEBUG_APPMENU='File/Nested'`, убрать ACTIVATE, задержка 1500 мс,
снимок 2500 мс. Путь задаётся точными видимыми подписями через `/`; это однократное
отладочное действие, не периодический опрос. Не задавать ACTIVATE для ручной проверки
неизвестных действий. Скриншоты с readback/PNG не использовать для оценки бюджета кадра.

## Ограничения замеров

Сессия хоста для FPS должна быть активной: запуск внутри приостановленной DRM-сессии
с другого VT даёт fallback ~24 Гц, что не характеризует производительность приложения.
Финальный release на активном Hyprland без скриншота во время замера:

| Метрика | Результат |
|---|---|
| Chromium `bench/anim.html` | 60,0–60,5 FPS по клиенту, 60 кадров/с по композитору |
| Сцена+GL Chromium, установившийся интервал | среднее 0,44 мс; пик 1,09 мс |
| Swap Chromium | среднее 0,04 мс |
| RssAnon композитора с Chromium | 57704 КиБ (~56,4 МиБ) |
| Простой Qt с открытым File и прекращённым миганием курсора | 0 тиков utime+stime за 5,000 с |
| RssAnon композитора с Qt/открытым меню | 58924 КиБ (~57,5 МиБ) |
| Появление File, включая подготовку содержимого | среднее сцена+GL 0,73 мс, пик 4,36 мс; CPU первого кадра 9,62 мс |

После исправления EGL ошибки в финальных логах Qt/GTK/Chromium отсутствовали, также
не обнаружены `wl_display.error`/protocol error клиентов. Вложенное окно было 944×490
физических пикселей, поэтому это не приёмка для 4K/DRM. Время debug-скриншота даёт большие
пики и удерживает память: с GTK после снимка RssAnon 73140 КиБ (~71,4 МиБ), это тоже
превышение, а не выполнение бюджета. В исходном прогоне внутри неактивной DRM-сессии
с открытым Qt-меню получен 1 тик/5 с; для итогового замера использован активный хост.

Строгие ≤1 мс и ≤60 МБ для полного сценария/целевого экрана ещё требуют работы:
см. [ANIMATION_CHECKS.md](ANIMATION_CHECKS.md). Отдельно остаётся тяжёлая CPU-подготовка
при старте/открытии панелей; D-Bus worker её не устраняет.

Gemini 4 Argon Medium через `agy` дал частичное статическое ревью worker: подтвердился
сценарий задержки следующего меню зависшим RPC, исправлен и проверен тестом. Доступ к
командам в headless CLI был запрещён; исходники передавались текстом. Полное ревью UI/EGL
Gemini не завершил; это не засчитывается как его проверка.

## Справочные интерфейсы

- [KDE Wayland appmenu](https://github.com/KDE/plasma-wayland-protocols/blob/master/src/protocols/appmenu.xml)
- [DBusMenu XML](https://github.com/KDE/plasma-workspace/blob/master/libdbusmenuqt/com.canonical.dbusmenu.xml)
- [Qt DBusMenu exporter](https://github.com/qt/qtbase/blob/dev/src/gui/platform/unix/dbusmenu/qdbusmenubar.cpp)
