#!/usr/bin/env bash
set -euo pipefail
BUNDLE_DIR="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
EXPECTED_ARCH="$(sed -n 's/^arch=//p' "$BUNDLE_DIR/BUILD-INFO.txt")"
if [[ "$(uname -m)" != "$EXPECTED_ARCH" ]]; then
    echo "Нужна архитектура $EXPECTED_ARCH; здесь $(uname -m)." >&2
    exit 1
fi
failed=0
for tool in ldd dbus-run-session fc-match; do
    if ! command -v "$tool" >/dev/null; then
        echo "Не найдена команда: $tool" >&2
        failed=1
    fi
done
(( failed == 0 )) || exit 1
for binary in "$BUNDLE_DIR"/usr/bin/feather-*; do
    # feather-session — скрипт, остальные файлы — доверенные бинарники этой сборки.
    [[ "${binary##*/}" != feather-session ]] || continue
    if ! report="$(LC_ALL=C ldd "$binary" 2>&1)" || [[ "$report" == *'not found'* ]]; then
        echo "Не выполнены зависимости ${binary##*/}:" >&2
        echo "$report" >&2
        failed=1
    fi
done
font="$(fc-match -f '%{file}' sans-serif)"
if [[ ! -r "$font" ]]; then
    echo 'Не найдены шрифты. Установите Noto Sans или Inter и fontconfig.' >&2
    failed=1
fi
if (( failed )); then
    echo 'Требования и ограничение glibc см. в README-TEST.md и BUILD-INFO.txt.' >&2
    exit 1
fi
echo 'Архитектура, связанные библиотеки, D-Bus и fontconfig: OK.'
echo 'Для запуска также нужны рабочий графический сеанс и драйвер OpenGL/EGL.'
