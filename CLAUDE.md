# Пёрышко (Feather DE)

Wayland-композитор на Smithay 0.7 в стиле macOS Tahoe. Цели, бюджеты, архитектура и дорожная
карта описаны в [docs/SPEC.md](docs/SPEC.md); любое изменение должно укладываться в бюджеты
из раздела 2. Общие правила для агентов — [AGENTS.md](AGENTS.md), очередь задач —
[docs/HANDOFF.md](docs/HANDOFF.md), хронология — [CHANGELOG.md](CHANGELOG.md) (после каждой
задачи добавляй запись сверху).

## Текущая передача от Codex, 2026-10-23

По просьбе пользователя сначала прочитай [docs/CLAUDE_HANDOFF.md](docs/CLAUDE_HANDOFF.md):
что уже закончено, результаты проверок, ограничения CC и порядок дальнейших задач.
Wi-Fi/звук/иконки CC реализованы, release собран, 31 тест прошёл; не начинать их заново.
Пользователь явно просит также использовать **Gemini 4 Argon через Antigravity CLI `agy`**.
Инструкция и проверенный вызов есть в передаче и в личном skill
`/home/zumka/.codex/skills/antigravity-subagent/SKILL.md`. Не обходить отказы permissions.

## Команды

- Сборка: `cargo build --release` (workspace: композитор и `files/` — файловый менеджер)
- Вложенный запуск: `target/release/feather-de --windowed`. Проверять активный VT/сокет:
  текущий Feather обычно wayland-2, Hyprland wayland-1; на неактивном хосте FPS не принимать.
  Не фиксировать устаревший wayland-0/labwc. Методика в передаче.
- Профайлер кадров: `FEATHER_PROFILE=1`
- Эталон FPS: `bench/anim.html` в Chromium с `--ozone-platform=wayland --enable-logging=stderr`

## Правила

- Цикл событий не блокировать. В простое не должно быть работы (0 % CPU).
- Фокус клавиатуры менять только через `refresh_focus()`, и никогда изнутри фильтра
  `KeyboardHandle::input` (дедлок): возвращайте `KeyAction`.
- CPU-буферы перерисовывать только при изменении, damage — только реальные области.
- Пользователь может работать из сессии самого Пёрышка: никогда не убивайте `feather-de`
  по имени (`pkill -x feather-de`) — это закроет все его окна. Тестовый экземпляр
  останавливайте по PID.
