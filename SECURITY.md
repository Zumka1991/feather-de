# Security policy

**[🇷🇺 Русский](#политика-безопасности)**

## Reporting a vulnerability

Please **do not open a public issue** for a security problem. Report it privately through
GitHub: [Report a vulnerability](https://github.com/Zumka1991/feather-de/security/advisories/new)
(Security tab → "Report a vulnerability").

Include the Feather version (`featherctl version`), how the problem can be reproduced and what an
attacker could achieve. You may write in English or Russian. You will get an answer as soon as
possible; please allow a few days.

## Supported versions

Only the latest release receives fixes. Feather is updated through `sudo apt upgrade` (Ubuntu) or
`sudo pacman -Syu` (Arch).

## What matters most

Feather runs with the privileges of the logged-in user, so the most interesting areas are the
D-Bus interfaces (`org.feather.Shell1`, the portal backend), the lock screen and the PAM rule,
the polkit agent, the screen-capture portal and anything that parses untrusted files (archives,
images, documents). Package integrity: the APT and pacman repositories are signed with the key
`D5E6 C262 8719 EF1D 6068 5758 C7F9 44C1 485C 5FC4`.

---

## Политика безопасности

О проблеме безопасности **не создавайте публичный issue**: сообщите приватно через GitHub
([Report a vulnerability](https://github.com/Zumka1991/feather-de/security/advisories/new),
вкладка Security). Укажите версию Пёрышка, шаги воспроизведения и возможные последствия.
Исправления выходят только для последней версии.
