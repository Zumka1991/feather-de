# Contributing to Feather DE (Пёрышко)

**[🇷🇺 Русский](#по-русски)** · **[← README](README.md)**

Thank you for your interest! Feather is a small project, and help of any size is welcome:
bug reports, translations, testing on real hardware, documentation and code.

## Ways to help

- **Report a bug** — use the [bug report form](https://github.com/Zumka1991/feather-de/issues/new/choose).
  The most useful details: the Feather version (`featherctl version`), your GPU and driver, how
  you installed it, and the steps to reproduce. You may write in English or Russian.
- **Ask a question or share an idea** — on the [forum](https://github.com/Zumka1991/feather-de/discussions).
- **Test on real hardware** — much of the DRM/KMS work (multiple monitors, hybrid graphics,
  touchpads, Fn keys) can only be verified on real machines. [docs/DRM_CHECKLIST.md](docs/DRM_CHECKLIST.md)
  lists what is still to be checked (in Russian; ask if you need help with it).
- **Translate the interface** — Feather speaks English, Russian, Spanish, German, French and
  Simplified Chinese. All interface strings use the Russian text as the key; translations live in
  [`crates/feather-ui/src/translations.tsv`](crates/feather-ui/src/translations.tsv)
  (`ru<TAB>en<TAB>es<TAB>de<TAB>fr<TAB>zh`, one line per string; `{}` marks a substituted value).
  Run the app with `FEATHER_I18N_MISSING=1` to print every string that has no translation.
  Fixing a wrong or awkward translation is a great first pull request.
- **Improve the documentation** — the English guide is [docs/USER_GUIDE.en.md](docs/USER_GUIDE.en.md).
  Most technical reports in `docs/` are still in Russian; English translations are welcome.

## Building

```bash
cargo build --release          # the compositor and all apps (a workspace)
cargo test --release --workspace
target/release/feather-de --windowed   # try it in a nested window
```

Feather needs Rust (stable), the development packages listed in
[packaging/install-ubuntu.sh](packaging/install-ubuntu.sh) and a Wayland session to run in.

## Before you send a pull request

- Read [docs/SPEC.en.md](docs/SPEC.en.md): every change must fit the performance and memory
  budgets in section 2, and the rules in section 3 ("Rules that must not be broken") apply — the
  event loop never blocks, idle CPU stays at 0 %.
- Run `cargo build --release --workspace` (no warnings) and `cargo test --release --workspace`.
- Keep commits focused. Describe what changed and how you checked it.
- Source comments and some project documents are in Russian; code and commit messages in English
  or Russian are both fine.

## License

Feather DE is licensed under GPL-3.0-or-later. By contributing you agree that your contribution
is released under the same license.

---

## По-русски

Спасибо за интерес! Пёрышко — небольшой проект, и помощь любого размера приветствуется:
сообщения об ошибках, переводы, проверка на настоящем железе, документация и код.

- **Об ошибке** — через [форму](https://github.com/Zumka1991/feather-de/issues/new/choose):
  версия (`featherctl version`), видеокарта и драйвер, способ установки, шаги воспроизведения.
- **Вопросы и идеи** — на [форуме](https://github.com/Zumka1991/feather-de/discussions).
- **Проверка на железе** — [docs/DRM_CHECKLIST.md](docs/DRM_CHECKLIST.md).
- **Переводы интерфейса** — [`crates/feather-ui/src/translations.tsv`](crates/feather-ui/src/translations.tsv),
  ключ — русский текст; `FEATHER_I18N_MISSING=1` печатает непереведённые строки.
- **Сборка:** `cargo build --release`, тесты: `cargo test --release --workspace`.
- **Перед pull request:** прочитайте [docs/SPEC.md](docs/SPEC.md) — изменение должно укладываться
  в бюджеты из раздела 2, цикл событий не блокируется, в простое 0 % CPU.
- Лицензия — GPL-3.0-or-later.
