#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="/home/zumka/Documents/antigravity/silly-tesla"
RELEASE_BIN="$PROJECT_DIR/target/release/feather-de"
DEBUG_BIN="$PROJECT_DIR/target/debug/feather-de"
KIOSK_CFG="$PROJECT_DIR/session/kiosk-config"
INNER_SCRIPT="$PROJECT_DIR/session/feather-session-inner.sh"

export PATH="$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:$PATH"
export XDG_SESSION_TYPE="wayland"
export XDG_CURRENT_DESKTOP="FeatherDE"
export QT_QPA_PLATFORM="wayland;xcb"
export GDK_BACKEND="wayland,x11"
export MOZ_ENABLE_WAYLAND="1"
export GTK_USE_PORTAL="1"
export PLASMA_INTEGRATION_USE_PORTAL="1"
export QT_QPA_PLATFORMTHEME="${QT_QPA_PLATFORMTHEME:-xdgdesktopportal}"
export ELECTRON_OZONE_PLATFORM_HINT="auto"

# 1. If launched inside an existing desktop session (nested mode), run feather-de directly:
if [[ -n "${WAYLAND_DISPLAY:-}" || -n "${DISPLAY:-}" ]]; then
    if [[ -x "$RELEASE_BIN" ]]; then
        exec "$RELEASE_BIN" "$@"
    elif [[ -x "$DEBUG_BIN" ]]; then
        exec "$DEBUG_BIN" "$@"
    else
        (cd "$PROJECT_DIR" && cargo build --release)
        exec "$RELEASE_BIN" "$@"
    fi
fi

# The host (labwc) draws the cursor at output scale 1, so on HiDPI screens the default
# 24 px cursor is tiny. Scale it like Feather scales its UI (4K → 2x, 1440p+ → 1.5x).
if [[ -z "${XCURSOR_SIZE:-}" ]]; then
    max_w=0
    for conn in /sys/class/drm/card*-*; do
        [[ "$(cat "$conn/status" 2>/dev/null)" == "connected" ]] || continue
        mode="$(head -n1 "$conn/modes" 2>/dev/null)"
        w="${mode%%x*}"
        [[ "$w" =~ ^[0-9]+$ ]] && (( w > max_w )) && max_w=$w
    done
    if (( max_w >= 3000 )); then
        export XCURSOR_SIZE=48
    elif (( max_w >= 2400 )); then
        export XCURSOR_SIZE=36
    else
        export XCURSOR_SIZE=24
    fi
fi
export XCURSOR_THEME="${XCURSOR_THEME:-breeze_cursors}"

# 2. Standalone session mode (from SDDM / Display Manager / TTY):
# Boot full hardware DRM/KMS + libinput + XWayland host in borderless kiosk mode
# labwc reads a per-user copy: Пёрышко rewrites it (mouse, touchpad, cursor from System
# Settings) and asks labwc to reload. The repository template is only the starting point.
RUNTIME_CFG="${XDG_CONFIG_HOME:-$HOME/.config}/feather-de/labwc"
mkdir -p "$RUNTIME_CFG"
[[ -f "$RUNTIME_CFG/rc.xml" ]] || cp "$KIOSK_CFG/rc.xml" "$RUNTIME_CFG/rc.xml"
exec labwc -C "$RUNTIME_CFG" -S "$INNER_SCRIPT"
