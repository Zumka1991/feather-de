#!/usr/bin/env bash
# Запуск распакованного тестового пакета без установки.
set -euo pipefail
BUNDLE_DIR="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
"$BUNDLE_DIR/check-dependencies.sh"
if [[ "${1:-}" == "--check" ]]; then
    exit 0
fi
if [[ -z "${WAYLAND_DISPLAY:-}" && -z "${DISPLAY:-}" ]]; then
    echo 'Запустите демо из терминала внутри графического сеанса Linux.' >&2
    exit 1
fi
# Раздельные настройки и данные; обычные файлы пользователя остаются доступны.
DEMO_HOME="${FEATHER_DEMO_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/feather-de-demo}"
export XDG_CONFIG_HOME="$DEMO_HOME/config"
export XDG_DATA_HOME="$DEMO_HOME/data"
export XDG_CACHE_HOME="$DEMO_HOME/cache"
export XDG_STATE_HOME="$DEMO_HOME/state"
mkdir -p "$XDG_CONFIG_HOME/feather-de" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_STATE_HOME"
if [[ ! -e "$XDG_CONFIG_HOME/feather-de/settings.conf" ]]; then
    # Без персональных закреплений разработчика; мастер приветствия доступен из меню.
    printf '%s\n' 'dock=builtin.launchpad,dolphin,konsole,builtin.notes,builtin.preview,builtin.textedit,builtin.calc,builtin.settings' > "$XDG_CONFIG_HOME/feather-de/settings.conf"
fi
export PATH="$BUNDLE_DIR/usr/bin:$PATH"
export XDG_DATA_DIRS="$BUNDLE_DIR/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
export QT_QPA_PLATFORM='wayland;xcb'
export GDK_BACKEND='wayland,x11'
export MOZ_ENABLE_WAYLAND=1
export ELECTRON_OZONE_PLATFORM_HINT=auto
# Оконный режим сохраняет дисплей хоста. Отдельная шина для приложений демо.
# --standalone/--drm запрещены: для полноценной сессии служит feather-session.
for arg in "$@"; do
    case "$arg" in
        --standalone|--drm|--login|--greeter)
            echo "Для демо недоступен параметр: $arg" >&2; exit 1 ;;
    esac
done
exec dbus-run-session -- "$BUNDLE_DIR/usr/bin/feather-de" --windowed "$@"
