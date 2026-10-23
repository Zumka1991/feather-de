#!/usr/bin/env bash
set -u

PROJECT_DIR="/home/zumka/Documents/antigravity/silly-tesla"
RELEASE_BIN="$PROJECT_DIR/target/release/feather-de"
DEBUG_BIN="$PROJECT_DIR/target/debug/feather-de"

dbus-update-activation-environment --systemd WAYLAND_DISPLAY DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE 2>/dev/null || true

while true; do
    rm -f /tmp/feather-restart
    if [[ -x "$RELEASE_BIN" ]]; then
        "$RELEASE_BIN" --standalone "$@"
    else
        "$DEBUG_BIN" --standalone "$@"
    fi

    # If /tmp/feather-restart was created (e.g. by reload-binary.sh during live development),
    # respawn the newly compiled feather-de binary immediately without logging out!
    if [[ -f /tmp/feather-restart ]]; then
        rm -f /tmp/feather-restart
        sleep 0.1
        continue
    fi
    break
done
