# Шрифты Пёрышка

Пёрышко носит свои шрифты, чтобы интерфейс выглядел одинаково на любой системе, что бы ни
стояло в fontconfig. Они загружаются напрямую из `<prefix>/share/feather-de/fonts`
(в сборке из исходников — из этой папки); системные шрифты нужны только как запасные
(иероглифы, эмодзи) и как замена, если пользователь поставил SF Pro / SF Mono.

| Файл | Где используется | Источник | Лицензия |
|---|---|---|---|
| `Inter-Regular.otf`, `Inter-SemiBold.otf` | весь интерфейс | Inter 4.1, пакет Ubuntu `fonts-inter` | OFL-1.1 — `Inter.copyright` |
| `JetBrainsMono-Regular.ttf`, `JetBrainsMono-Bold.ttf` | Терминал | JetBrains Mono 2.304, пакет `fonts-jetbrains-mono` | Apache-2.0 (апстрим — OFL-1.1) — `JetBrainsMono.copyright` |
| `NotoSansSymbols2-Regular.ttf` | символы, которых нет в Inter (⏻ …) | Noto 2020-12, пакет `fonts-noto-core` | OFL-1.1 — `NotoSansSymbols2.copyright` |
