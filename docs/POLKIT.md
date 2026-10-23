# Собственный polkit-агент — 2026-10-24

Пёрышко само является агентом аутентификации polkit своей сессии. Окно пароля рисует
композитор (`src/auth_dialog.rs`): ни один клиент не может нарисовать поверх него,
подделать его или получить нажатия, пока оно открыто (фокус клавиатуры снят, все клики
поглощаются; проходят только Ctrl+Alt-сочетания VT).

## Устройство

- `src/polkit_agent.rs`: системная шина в потоке `feather-polkit`, объект
  `/org/feather/PolicyKit1/AuthenticationAgent`, регистрация `RegisterAuthenticationAgent`
  для `unix-session` из `XDG_SESSION_ID` (иначе `unix-process`). Вызовы принимаются
  только от текущего владельца `org.freedesktop.PolicyKit1`. Не больше 8 запросов.
- Identities: `unix-user` и участники `unix-group` (getpw*/getgr*_r). По умолчанию —
  текущий пользователь; несколько — переключение кликом по полю или Tab.
- Пароль проверяет root-помощник polkit: сокет `/run/polkit/agent-helper.socket`
  (polkit ≥ 126) или setuid `polkit-agent-helper-1`. Протокол libpolkit-agent:
  `user\n`, `cookie\n`, строки `PAM_PROMPT_ECHO_OFF/ON`, `PAM_ERROR_MSG`, `PAM_TEXT_INFO`,
  `SUCCESS`/`FAILURE`. Помощник сам сообщает polkitd об успехе. Разговор — в отдельном
  потоке; дополнительные вопросы PAM (код и т. п.) показываются в том же окне.
- Пароль хранится в `Zeroizing<String>`, не логируется, после передачи стирается.
- Неверный пароль: сообщение PAM или «Неверный пароль», красная рамка, встряхивание
  (GPU-сдвиг 0,45 с), поле очищается, можно повторить. Esc/«Отменить» — `Cancelled`.
  `CancelAuthentication` от polkitd закрывает окно. Очередь, если запросов несколько.
- Включение: в DRM-сессии (`--standalone`) всегда. Перед регистрацией останавливаются
  lxqt/KDE-агенты **только этой** сессии (по `XDG_SESSION_ID` в их окружении). Если
  зарегистрироваться не удалось — прежний запуск lxqt-policykit-agent.
  `FEATHER_POLKIT=lxqt` — сразу внешний агент; `FEATHER_POLKIT_AGENT=1` — свой во
  вложенном режиме (регистрация для сессии хоста обычно отклоняется: там свой агент).

## Проверка

- `cargo test --release --bin feather-de polkit`: протокол с поддельным помощником
  (верный/неверный пароль, второй вопрос PAM, закрытие без ответа), разбор identities,
  g_strcompress.
- Вложенный сценарий без D-Bus и без настоящего PAM (короткий путь для сокета):
  `FEATHER_DEBUG_POLKIT=$(mktemp -d /tmp/fpk.XXXX) FEATHER_DEBUG_INPUT_ONLY=1
  target/release/feather-de --windowed --autostart "feather-terminal -e sleep 30"` —
  неверный пароль встряхивает окно и не закрывает его, верный закрывает, клиент под
  окном не получает фокус клавиатуры. Снимки с `FEATHER_DEBUG_SCREENSHOT_MS=1350/2650`.
- **Не проверено:** настоящий polkitd и PAM — только пользователем в DRM-сессии,
  например `pkexec true`. Лог: `~/.local/state/feather-de/drm.log` («polkit: …»).
