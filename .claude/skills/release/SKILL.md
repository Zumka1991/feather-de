---
name: release
description: Выпуск новой версии Пёрышка (feather-de) в APT-репозиторий на GitHub Pages и в GitHub Release — версия, CHANGELOG, тег, CI, проверка apt. Используй, когда пользователь просит «выпустить», «сделать релиз», «опубликовать версию», «обновить репозиторий» Пёрышка.
---

# Выпуск Пёрышка

Выпуск = тег `vX.Y.Z` на `master`. Остальное делает CI (`.github/workflows/release.yml`,
~6–8 минут): собирает `.deb` на ubuntu-24.04 (вместе с xwayland-satellite), подписывает
репозиторий секретом `APT_SIGNING_KEY`, публикует ветку `gh-pages`
(https://zumka1991.github.io/feather-de: репозиторий, `install.sh`, страница установки)
и создаёт GitHub Release с `feather-de_X.Y.Z_amd64.deb` и `feather-de_amd64.deb`.
Установленные системы получают версию через `sudo apt upgrade`. ISO Пёрышко OS (~/feather-os)
пересобирать не нужно.

## Правила

- Отправку в `master` и тег делать только с явного согласия пользователя в этом разговоре
  («да, выпускай»). Согласие на прошлый выпуск не переносится на следующий.
- Всё идёт прямо в `master` (так хочет пользователь), без PR.
- Никогда не печатать и не трогать закрытый ключ; публичный — `packaging/apt/feather-de.asc`,
  отпечаток `D5E6 C262 8719 EF1D 6068 5758 C7F9 44C1 485C 5FC4`.
- Не делать force-push в `master`. Тег удалять/переставлять только если CI упал до
  публикации и пользователь согласен.

## Шаги

1. **Состояние.** `git status -sb` — рабочее дерево чистое (или осталось закоммитить только
   выпускаемые изменения), ветка `master`, `git fetch` и нет расхождения с `origin/master`.
   Последний тег: `git describe --tags --abbrev=0`; что войдёт: `git log --oneline vПРЕД..HEAD`.

2. **Проверка.** Без предупреждений и падений:
   ```bash
   cargo build --release --workspace
   cargo test --release --workspace
   ```
   Если менялась упаковка (`packaging/`), проверить пакет в чистой Ubuntu (без root, ~5 мин):
   ```bash
   ./packaging/build-deb.sh --out /tmp/feather-dist
   ./packaging/test-install-chroot.sh /tmp/feather-dist/feather-de_*_amd64.deb
   ```

3. **Версия.** Исправления — `Z+1`, новые возможности — `Y+1`. Меняется только корневой
   `Cargo.toml` (`version = "X.Y.Z"`, первая строка `version`), затем `cargo build --release`
   обновит `Cargo.lock` (одна строка пакета feather-de). Версии подпакетов не трогать.

4. **CHANGELOG.md.** Сверху (под шапкой) запись в стиле прошлых выпусков:
   ```
   ## ГГГГ-ММ-ДД — Выпуск X.Y.Z (Claude)

   - Что получат пользователи, коротко, по пунктам; новые зависимости пакета — apt поставит сам.
   ```

5. **Коммит и тег** (после согласия):
   ```bash
   git commit -am "release: X.Y.Z" -m "Краткий список изменений по-английски." -m "Co-Authored-By: …"
   git tag -a vX.Y.Z -m "Пёрышко X.Y.Z"
   git push origin master && git push origin vX.Y.Z
   ```
   Тег обязан совпадать с версией в `Cargo.toml`, иначе CI остановится на первом шаге.

6. **CI.** `gh run list --limit 3` → id запуска «Выпуск» для тега; ждать в фоне:
   `gh run watch ID --exit-status` (run_in_background, не опрашивать в цикле). Если упал —
   `gh run view ID --log-failed`, исправить, новым коммитом; тег переставить только с согласия
   пользователя (`git tag -d vX.Y.Z && git push --delete origin vX.Y.Z`, затем шаг 5).

7. **Проверка публикации** (Pages обновляется ещё ~1 минуту после CI):
   ```bash
   curl -fsS https://zumka1991.github.io/feather-de/dists/stable/main/binary-amd64/Packages | grep ^Version
   curl -fsSO https://zumka1991.github.io/feather-de/dists/stable/InRelease
   gpg --dearmor < packaging/apt/feather-de.asc > /tmp/k.gpg && gpgv --keyring /tmp/k.gpg InRelease
   curl -fsS https://zumka1991.github.io/feather-de/install.sh | cmp - packaging/get.sh
   gh release view vX.Y.Z --json assets -q '.assets[].name'
   ```
   В Packages должна быть новая версия (и до двух прошлых), подпись — «Действительная».

8. **Отчёт пользователю** (по-русски): версия, что вошло, что обновиться можно
   `sudo apt update && sudo apt upgrade`, затем выйти из сеанса и войти снова. Новым
   пользователям: `curl -fsSL https://zumka1991.github.io/feather-de/install.sh | bash`
   или `.deb` по ссылке `https://github.com/Zumka1991/feather-de/releases/latest/download/feather-de_amd64.deb`.
