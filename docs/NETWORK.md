# Сеть (Wi‑Fi, сохранённые профили, Ethernet, VPN) — 2026-10-25

Реализован раздел «Системные настройки → Сеть» (`Page::Network`) и расширен фоновый
поток `feather-network` ([src/controls/network.rs](../src/controls/network.rs),
[src/controls.rs](../src/controls.rs), [src/system_settings.rs](../src/system_settings.rs)).

## Поведение

«Системные настройки → Сеть» объединяют четыре секции в одном окне:

1. **Wi‑Fi**:
   - Включение/выключение радиомодуля (`WirelessEnabled`), кнопка «Поиск сетей» (`RequestScan`),
     список доступных точек доступа с группировкой по `(SSID, Security)` и выбором сильнейшего сигнала.
   - Подключение к сохранённым и открытым сетям (`Open` / `Owe`) одним кликом (`ActivateConnection` /
     `AddAndActivateConnection`).
   - Встроенная модальная шторка ввода пароля для сетей WPA2/WPA3 (`Security::Psk | Security::Sae | Security::Transition`)
     с маскированием (`TextInput::draw_masked` / `click_masked`), автоповтором клавиш (`key_repeat::Target::Settings`)
     и передачей секрета через `Zeroizing<String>` (`take_text()`) с лимитом 64 байта.
   - Переход в `nm-connection-editor` (или `nmtui` в терминале) для корпоративных сетей 802.1X (`Security::Other`).
2. **Сохранённые сети Wi‑Fi (`SavedWifi`)**:
   - Чтение профилей `802-11-wireless` из `/org/freedesktop/NetworkManager/Settings` (`ListConnections` /
     свойство `Connections` с резервным чтением `AvailableConnections` адаптера).
   - Отображение статуса («Подключено», «В зоне действия», «Вне зоны действия»), типа защиты и режима
     автоподключения.
   - Переключение «Авто» / «Вручную» (`Control::SetAutoconnect`): читает `GetSettings`, подтягивает
     существующие секреты `GetSecrets("802-11-wireless-security")`, чтобы `Update` не стирал сохранённый
     пароль PSK/SAE, и обновляет `connection.autoconnect`.
   - Удаление профиля и пароля кнопкой «Забыть» (`Control::ForgetConnection` → `Settings.Connection.Delete`).
3. **Ethernet (`Ethernet`)**:
   - Опрос проводных адаптеров (`DeviceType == 1`) и интерфейса `org.freedesktop.NetworkManager.Device.Wired`
     (`Carrier`, `Speed` в Мбит/с).
   - Отображение состояния линка («Подключено», «Подключение…», «Кабель подключён», «Кабель не подключён»)
     и скорости соединения.
   - Отключение активного адаптера (`Device.Disconnect`) и повторное подключение (`ActivateConnection`
     с сохранённым профилем или автоматическим выбором `'/'`).
4. **VPN и WireGuard (`Vpn`)**:
   - Сбор профилей `vpn` и `wireguard` из `Settings` и сопоставление с `ActiveConnections`
     (`org.freedesktop.NetworkManager.Connection.Active`, свойства `State` и `VpnState`).
   - Включение (`Control::ConnectVpn` → `ActivateConnection(profile, "/", "/")`) и отключение
     (`Control::DisconnectVpn` → `DeactivateConnection(active_path)`).
5. **Сервисные действия**:
   - «Обновить состояние сети» (`Control::RefreshNetwork`) и «Расширенные настройки сети…»
     (`Action::OpenConnectionEditor`).
   - Кнопка «Настройки сети…» в подпанели Wi‑Fi Пункта управления (`ShellClickAction::NetworkSettings`)
     закрывает Пункт управления и открывает `Системные настройки → Сеть`.

## Выполнение и обновления

- Вся работа с D‑Bus (`zbus`) остаётся в фоновом потоке `feather-network` с таймаутом 3 с на вызов;
  главный цикл событий композитора никогда не блокируется.
- Режим `detailed = true` включается только когда открыта подпанель Wi‑Fi/пароля в Пункте управления
  или видима страница `Системные настройки → Сеть` (`controls.sync`). В закрытом состоянии (`detailed = false`)
  фоновый поток не опрашивает точки доступа, сохранённые профили, проводные адаптеры и VPN.
- Фильтр сигналов `relevant(message, detailed)` отслеживает `PropertiesChanged` для `NM`, `DEVICE`,
  `WIRED`, `ACTIVE_CONN`, `SETTINGS_IFACE`, `WIRELESS`, `AP`, а также сигналы `NewConnection`,
  `ConnectionRemoved`, `Updated`, `Removed` и `VpnStateChanged`.
- Ошибки команд (`Update::Error("Wi‑Fi", _)`) доставляются в открытое окно настроек и не затираются
  последующими фоновыми снимками до явного действия пользователя.

## Проверки

```sh
cargo build --release
dbus-run-session -- env FEATHER_SETTINGS_FIXTURES=/tmp/feather-network-fixtures \
  cargo test --release --workspace -- --include-ignored
ss -xlp | grep wayland-
python3 bench/network-settings-check.py --host wayland-1 --output /tmp/feather-network-quiet --protocol-summary
python3 bench/network-settings-check.py --host wayland-1 --output /tmp/feather-network-e2e --screenshot --protocol-summary
```

- `cargo build --release`: 0 ошибок, 0 предупреждений.
- `cargo test --release --workspace -- --include-ignored`: **184 теста прошли** (127 в `feather-de`,
  16 в `feather-files`, 5 в `feather-notes`, 26 в `feather-terminal`, 10 в `feather-ui`).
  Приватный D‑Bus тест `controls::network::tests::private_bus_snapshot_and_actions_use_real_nm_wire_types_and_reject_stale_networks`
  проверяет реальные сигнатуры `zvariant` для `SavedWifi`, `Ethernet`, `Vpn`, сохранение секрета при
  `SetAutoconnect`, `ForgetConnection`, `Disconnect`, `ConnectEthernet`, `DisconnectVpn` и `ConnectVpn`.
- Рендер-фикстуры `Network` и `Network-overlay` проверены в светлой и тёмной темах при масштабах `1.0`,
  `1.25` и `2.0`.
- Сквозной сценарий `bench/network-settings-check.py` запускает вложенный композитор с
  `FEATHER_DEBUG_INPUT_ONLY=1` и `FEATHER_DEBUG_NETWORK_SETTINGS=1` поверх приватной шины
  `dbus-run-session` (`DBUS_SYSTEM_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS`), проверяет переход из
  Пункта управления в «Системные настройки → Сеть», переключение автоподключения с сохранением PSK,
  удаление профиля Wi‑Fi, отключение и подключение Ethernet, переключение двух профилей VPN/WireGuard,
  ввод пароля WPA2 через встроенную шторку и сохранение ошибки команды при фоновом сигнале.

### Замеры бюджетов (×2, 2880×1800, `FEATHER_DEBUG_INPUT_ONLY=1`)

| Сценарий | Пик RssAnon | Установившийся RssAnon | CPU за ≥ 5 с простоя | Команд D‑Bus |
|---|---:|---:|---:|---:|
| Сеть, без снимка экрана | 47,48 МиБ | 37,46 МиБ | 0 тиков / 5,03 с | 8 |
| Сеть, со снимком экрана | 97,68 МиБ | 58,12 МиБ | 0 тиков / 5,03 с | 8 |

Установившаяся память `RssAnon` в обоих прогонах укладывается в бюджет ≤ 60 МБ, простой после
завершения сценария — строго `0` тиков CPU за `5,03 с`. Рабочая сессия пользователя не
перезапускалась и не затрагивалась.
