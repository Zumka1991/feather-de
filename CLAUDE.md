# Пёрышко (Feather DE)

Wayland-композитор на Smithay 0.7 в стиле macOS Tahoe. Цели, бюджеты, архитектура и дорожная
карта описаны в [docs/SPEC.md](docs/SPEC.md); любое изменение должно укладываться в бюджеты
из раздела 2. Общие правила для агентов — [AGENTS.md](AGENTS.md), очередь задач —
[docs/HANDOFF.md](docs/HANDOFF.md), хронология — [CHANGELOG.md](CHANGELOG.md) (после каждой
задачи добавляй запись сверху).

## Команды

- Сборка: `cargo build --release`
- Вложенный запуск: `target/release/feather-de --windowed` (замеры только на хосте labwc:
  `WAYLAND_DISPLAY=wayland-0`, иначе программный рендер)
- Профайлер кадров: `FEATHER_PROFILE=1`
- Эталон FPS: `bench/anim.html` в Chromium с `--ozone-platform=wayland --enable-logging=stderr`

## Правила

- Цикл событий не блокировать. В простое не должно быть работы (0 % CPU).
- Фокус клавиатуры менять только через `refresh_focus()`, и никогда изнутри фильтра
  `KeyboardHandle::input` (дедлок): возвращайте `KeyAction`.
- CPU-буферы перерисовывать только при изменении, damage — только реальные области.
- Пользователь может работать из сессии самого Пёрышка: не убивайте `feather-de` по имени
  (`pkill -x feather-de`) без его согласия — это закроет все его окна. Тестовый экземпляр
  останавливайте по PID.
