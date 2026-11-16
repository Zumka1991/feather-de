#!/usr/bin/env bash
set -euo pipefail

# Messages: Russian under a Russian locale, English otherwise
case "${LC_ALL:-${LC_MESSAGES:-${LANG:-}}}" in ru*) _L=ru ;; *) _L=en ;; esac
t() { if [[ $_L == ru ]]; then printf '%s' "$1"; else printf '%s' "$2"; fi; }
BUNDLE_DIR="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
EXPECTED_ARCH="$(sed -n 's/^arch=//p' "$BUNDLE_DIR/BUILD-INFO.txt")"
if [[ "$(uname -m)" != "$EXPECTED_ARCH" ]]; then
    echo "$(t "Нужна архитектура $EXPECTED_ARCH; здесь $(uname -m)." "Architecture $EXPECTED_ARCH is required; this is $(uname -m).")" >&2
    exit 1
fi
failed=0
for tool in ldd dbus-run-session fc-match; do
    if ! command -v "$tool" >/dev/null; then
        echo "$(t "Не найдена команда: $tool" "Command not found: $tool")" >&2
        failed=1
    fi
done
(( failed == 0 )) || exit 1
for binary in "$BUNDLE_DIR"/usr/bin/feather-*; do
    # feather-session — скрипт, остальные файлы — доверенные бинарники этой сборки.
    [[ "${binary##*/}" != feather-session ]] || continue
    if ! report="$(LC_ALL=C ldd "$binary" 2>&1)" || [[ "$report" == *'not found'* ]]; then
        echo "$(t "Не выполнены зависимости ${binary##*/}:" "Unmet dependencies of ${binary##*/}:")" >&2
        echo "$report" >&2
        failed=1
    fi
done
font="$(fc-match -f '%{file}' sans-serif)"
if [[ ! -r "$font" ]]; then
    echo "$(t 'Не найдены шрифты. Установите Noto Sans или Inter и fontconfig.' 'No fonts found. Install Noto Sans or Inter and fontconfig.')" >&2
    failed=1
fi
if (( failed )); then
    echo "$(t 'Требования и ограничение glibc см. в README-TEST.md и BUILD-INFO.txt.' 'See README-TEST.md and BUILD-INFO.txt for the requirements and the glibc limit.')" >&2
    exit 1
fi
echo "$(t 'Архитектура, связанные библиотеки, D-Bus и fontconfig: OK.' 'Architecture, linked libraries, D-Bus and fontconfig: OK.')"
echo "$(t 'Для запуска также нужны рабочий графический сеанс и драйвер OpenGL/EGL.' 'Running also needs a working graphical session and an OpenGL/EGL driver.')"
