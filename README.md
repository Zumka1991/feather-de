# Пёрышко (Feather DE)

**Пёрышко (Feather DE)** — сверхбыстрое и лёгкое окружение рабочего стола для Wayland на Rust (`Smithay 0.7`, `tiny-skia`) в эстетике **macOS Tahoe**.

## Ключевые показатели (бюджеты `docs/SPEC.md`)

- **0 % CPU в простое** — строго событийный цикл `calloop`, без фонового поллинга и перерисовок.
- **≤ 1 мс работы композитора на кадр** — аппаратная сборка сцены GL/DRM с точным `damage tracking`.
- **≤ 60 МБ приватной памяти (`RssAnon`)** — единый исполняемый файл композитора со встроенными системными панелями.

## Состав системы

- `feather-de` — композитор Wayland/DRM/KMS, верхняя строка меню (с глобальным меню AppMenu/GTK), Dock с пружинной физикой, Пункт управления (Wi-Fi, Bluetooth, звук PipeWire, яркость, Night Shift, Фокус), Spotlight, Launchpad, Mission Control и рабочие столы, Системные настройки, Мастер приветствия при первом запуске, экран входа (`--login` для `greetd`), экран блокировки (`ext-session-lock-v1` + встроенный PAM), заставка загрузки (`--boot-splash`), агент Polkit и системный портал `xdg-desktop-portal` (`FileChooser`, `Screenshot`, `ScreenCast`, `Access`).
- `feather-files` — файловый менеджер «Файлы» (Finder-стиль, вкладки, Quick Look по `Пробелу`, корзина, перетаскивание Wayland DnD, системный диалог открытия/сохранения).
- `feather-terminal` — эмулятор терминала «Терминал» (256-цветный/TrueColor VT, вкладки, поиск, выделение и буфер обмена).
- `feather-notes` — приложение «Заметки» с мгновенным автосохранением и поиском.
- `feather-preview` — «Просмотр» изображений (`PNG`, `JPEG`, `WebP`, `SVG`) и многостраничных документов `PDF`.
- `feather-textedit` — «Текстовый редактор» с вкладками, подсветкой синтаксиса, поиском и заменой.
- `feather-calc` — «Калькулятор» с обычным, инженерным и программистским режимами и журналом вычислений.

## Сборка и установка

```bash
# Сборка всех компонентов в release:
make build

# Установка в систему (требуются права root, поддерживает DESTDIR и PREFIX):
sudo make install

# Локальная установка в ~/.local:
./packaging/install.sh --user

# Сборка релизного архива в dist/:
make release

# Сборка пакета Arch Linux (pacman):
cd packaging && makepkg -si
```

## Запуск

- **Прямой запуск на видеокарте (DRM/KMS)**: выберите сессию **«Пёрышко (прямой вывод DRM/KMS)»** на экране входа или запустите `feather-session` (`session/start-feather-drm.sh`) из свободной TTY (`Ctrl+Alt+F3`).
- **Оконный режим для разработки и проверки**: `target/release/feather-de --windowed`.
