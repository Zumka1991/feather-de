#!/usr/bin/env bash
# Пёрышко напрямую на видеокарте (DRM/KMS), без labwc посередине.
# Запуск: из экрана входа (сессия «Пёрышко (прямой вывод)») или со свободной TTY.
# Аварийный выход: Ctrl+Alt+Backspace. Переключение консолей: Ctrl+Alt+F1…F12.
set -u

PROJECT_DIR="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
BIN="$PROJECT_DIR/target/release/feather-de"
LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/feather-de"
mkdir -p "$LOG_DIR"

export XDG_SESSION_TYPE="wayland"
export XDG_CURRENT_DESKTOP="FeatherDE"
export QT_QPA_PLATFORM="wayland;xcb"
export GDK_BACKEND="wayland,x11"
export MOZ_ENABLE_WAYLAND="1"
export GTK_USE_PORTAL="1"
export PLASMA_INTEGRATION_USE_PORTAL="1"
export QT_QPA_PLATFORMTHEME="${QT_QPA_PLATFORMTHEME:-xdgdesktopportal}"
export ELECTRON_OZONE_PLATFORM_HINT="auto"
export XCURSOR_THEME="${XCURSOR_THEME:-breeze_cursors}"
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:$PATH"
# A TTY login may lack the session bus address even though systemd provides the bus
if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" && -S "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/bus" ]]; then
    export DBUS_SESSION_BUS_ADDRESS="unix:path=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/bus"
fi
# Seat management goes through systemd-logind (seatd is not running on this system);
# libseat's auto-detection otherwise fails with ENOSYS
export LIBSEAT_BACKEND="${LIBSEAT_BACKEND:-logind}"
# We are the compositor: never inherit a parent's display
unset WAYLAND_DISPLAY DISPLAY

if [[ ! -x "$BIN" ]]; then
    (cd "$PROJECT_DIR" && cargo build --release) || exit 1
fi

while true; do
    rm -f /tmp/feather-restart
    echo "=== $(date) ===" >> "$LOG_DIR/drm.log"
    RUST_LOG="${RUST_LOG:-feather_de=info,warn}" "$BIN" --drm --standalone "$@" >> "$LOG_DIR/drm.log" 2>&1
    # reload-binary.sh asks for a restart with the freshly built binary
    if [[ -f /tmp/feather-restart ]]; then
        continue
    fi
    break
done
