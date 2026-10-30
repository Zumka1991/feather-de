# Bluetooth (BlueZ): включение, поиск, подключение — 2026-10-25

Реализованы подпанель Bluetooth в Пункте управления (`control_panel::Page::Bluetooth`),
раздел «Системные настройки → Bluetooth» (`system_settings::Page::Bluetooth`) и
событийный фоновый поток `feather-bluetooth` ([src/controls/bluetooth.rs](../src/controls/bluetooth.rs),
[src/controls.rs](../src/controls.rs), [src/control_panel.rs](../src/control_panel.rs),
[src/system_settings.rs](../src/system_settings.rs)).

## Поведение

1. **Пункт управления (`control_panel::Page::Bluetooth`)**:
   - Плитка `Bluetooth` на главной странице Пункта управления показывает живое состояние адаптера
     (`Вкл.` / `Выкл.` / `Недоступно` / `Поиск…` / имя подключённого устройства или число активных
     подключений).
   - Нажатие на плитку открывает подпанель `Page::Bluetooth` и запускает поиск (`Control::ScanBluetooth`).
   - Внутри подпанели доступны переключатель питания адаптера (`Powered`), кнопка запуска/остановки
     поиска (`StartDiscovery` / `StopDiscovery`), прокручиваемый список устройств с индикацией заряда
     батареи (`org.bluez.Battery1.Percentage`) и подключением/отключением в один клик, а также нижняя
     кнопка «Настройки Bluetooth…» (`ShellClickAction::BluetoothSettings`), открывающая
     `Системные настройки → Bluetooth`.
2. **«Системные настройки → Bluetooth» (`system_settings::Page::Bluetooth`)**:
   - Карточка заголовка с пиктограммой Bluetooth и переключателем питания адаптера (`Control::Bluetooth(bool)`).
   - Строка «Поиск устройств» / «Остановить поиск устройств» (`Control::ScanBluetooth`).
   - Раздел **«Мои устройства»** (всё, что BlueZ помнит: сопряжённые, доверенные и подключённые,
     `BtDevice::remembered`): тип аксессуара (`Наушники`,
     `Клавиатура`, `Мышь или трекпад`, `Гарнитура`, `Аудиоустройство`, `Геймпад` и др.), статус
     (`Подключено` / `Подключение…` / `Сопряжено` / `Требуется повторное сопряжение` — доверенное
     устройство с потерянным ключом), процент заряда батареи (`org.bluez.Battery1`),
     кнопки «Отключить» / «Подключить» / «Отменить» (`DisconnectBluetooth` / `ConnectBluetooth`) и
     «Забыть» (`ForgetBluetooth` → `org.bluez.Adapter1.RemoveDevice`), доступная и во время подключения.
   - Раздел **«Другие устройства»** (обнаруженные несопряжённые аксессуары): при нажатии «Подключить»
     выполняет `Pair` (если устройство ещё не сопряжено), выставляет `Trusted = true` и вызывает
     `Connect`; пока попытка идёт, кнопка «Отменить» прерывает её. Безымянные BLE‑маяки с именем‑заглушкой вида `XX-XX-XX-XX-XX-XX` без иконки
     отфильтровываются (`is_mac_placeholder`), чтобы не засорять список.
   - Сервисные строки «Обновить состояние Bluetooth» (`Control::RefreshBluetooth`) и
     «Расширенный диспетчер Bluetooth…» (`Action::OpenBluetoothManager` → `blueman-manager`).

## Выполнение и обновления

- Вся работа с системной шиной D‑Bus (`org.bluez`) выполняется в отдельном фоновом потоке
  `feather-bluetooth` (стек 256 КиБ) без блокировки главного цикла событий композитора:
  - Чтение дерева объектов за один вызов `org.freedesktop.DBus.ObjectManager.GetManagedObjects` на `/`;
    таймаут любого вызова — 5 с (`method_timeout` соединения).
  - Пока идут `Pair` / `Connect`, поток продолжает читать команды (`connect_cancellable`):
    `DisconnectBluetooth` («Отменить», повторное нажатие в Пункте управления), `ForgetBluetooth` или
    выключение адаптера для этого устройства прерывают попытку (`CancelPairing` + `Disconnect`),
    прочие команды ждут в очереди. После каждого действия поток присылает состояние, даже если оно не
    изменилось, чтобы интерфейс снял «Подключение…».
  - Подписка на сигналы `InterfacesAdded`, `InterfacesRemoved`, `PropertiesChanged` (`org.bluez.Adapter1`,
    `org.bluez.Device1`, `org.bluez.Battery1`) и `NameOwnerChanged` — без периодических таймеров и опроса.
- Когда закрыты и подпанель Bluetooth в Пункте управления, и страница `Системные настройки → Bluetooth`,
  `controls.sync` отправляет `WatchBluetooth(false)`:
  - Если поиск (`StartDiscovery`) был запущен из Пёрышка, поток автоматически вызывает `StopDiscovery`.
  - В закрытом состоянии фильтр `relevant(message, false)` игнорирует фоновый шум `RSSI` от `org.bluez.Device1`,
    обеспечивая строго 0 % CPU в простое.

## Проверки

```sh
cargo build --release
dbus-run-session -- env FEATHER_SETTINGS_FIXTURES=/tmp/feather-bt-fixtures \
  cargo test --release --workspace -- --include-ignored
ss -xlp | grep wayland-
python3 bench/bluetooth-settings-check.py --host wayland-1 --output /tmp/feather-bt-e2e
python3 bench/bluetooth-settings-check.py --host wayland-1 --output /tmp/feather-bt-e2e-shot --screenshot --protocol-summary
```

- `cargo build --release`: 0 ошибок, 0 предупреждений.
- `cargo test --release --workspace -- --include-ignored`: **188 тестов прошли** (131 в `feather-de`,
  16 в `feather-files`, 5 в `feather-notes`, 26 в `feather-terminal`, 10 в `feather-ui`), включая:
  - `controls::bluetooth::tests::mac_placeholder_filter_hides_unnamed_ble_beacons_but_keeps_named_or_paired_devices`
  - `controls::bluetooth::tests::signal_filter_ignores_background_rssi_when_bluetooth_view_is_closed`
  - `controls::bluetooth::tests::private_bus_bluez_object_manager_and_device_actions`
  - `controls::bluetooth::tests::private_bus_forget_trusted_unpaired_device_cancels_pending_pairing`
  - `control_panel::tests::bluetooth_page_toggles_scans_and_connects_devices`
- Проверены рендер‑фикстуры `Bluetooth` в светлой и тёмной темах при масштабах `1.0`, `1.25` и `2.0`.
- Сквозной сценарий [`bench/bluetooth-settings-check.py`](../bench/bluetooth-settings-check.py)
  запускает вложенный композитор с `FEATHER_DEBUG_INPUT_ONLY=1` и `FEATHER_DEBUG_BLUETOOTH_SETTINGS=1`
  поверх приватной шины `dbus-run-session` с mock‑сервисом `org.bluez`, проверяет открытие подпанели
  Bluetooth в Пункте управления и запуск `StartDiscovery`, фильтрацию безымянного BLE‑маяка, переход
  в «Системные настройки → Bluetooth», сопряжение + `Trusted = true` + подключение нового устройства,
  отключение активных наушников, удаление клавиатуры через `RemoveDevice`, сохранение ошибки команды
  при фоновом сигнале и остановку поиска `StopDiscovery`.

### Замеры бюджетов (×2, 2880×1800, `FEATHER_DEBUG_INPUT_ONLY=1`)

| Сценарий | Пик RssAnon | Установившийся RssAnon | CPU за ≥ 5 с простоя | Команд D‑Bus |
|---|---:|---:|---:|---:|
| Bluetooth, без снимка экрана | 47,96 МиБ | 38,10 МиБ | 0 тиков / 5,04 с | 8 |
| Bluetooth, со снимком экрана | 92,62 МиБ | 53,06 МиБ | 0 тиков / 5,03 с | 8 |

Установившаяся память `RssAnon` в обоих прогонах укладывается в бюджет ≤ 60 МБ, простой после
завершения сценария — строго `0` тиков CPU за `> 5 с`. Рабочая сессия пользователя не
перезапускалась и не затрагивалась.
