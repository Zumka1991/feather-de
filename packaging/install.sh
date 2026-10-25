#!/usr/bin/env bash
# Универсальный установщик Пёрышка (Feather DE)
# Поддерживает:
#   - системную установку (PREFIX=/usr DESTDIR=/pkg ./packaging/install.sh)
#   - локальную установку в профиль пользователя (./packaging/install.sh --user)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
PREFIX="${PREFIX:-/usr}"
SYSCONFDIR="${SYSCONFDIR:-/etc}"
DESTDIR="${DESTDIR:-}"
USER_MODE=0
NO_BUILD=0
TARGET_DIR="$ROOT_DIR/target/release"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --user)
            USER_MODE=1
            PREFIX="$HOME/.local"
            shift
            ;;
        --no-build)
            NO_BUILD=1
            shift
            ;;
        --target-dir)
            TARGET_DIR="$2"
            shift 2
            ;;
        --destdir)
            DESTDIR="$2"
            shift 2
            ;;
        --prefix)
            PREFIX="$2"
            shift 2
            ;;
        *)
            echo "Неизвестный параметр: $1" >&2
            exit 1
            ;;
    esac
done

if [[ "$NO_BUILD" -eq 0 ]]; then
    echo "==> Сборка Пёрышка (release, все программы рабочего окружения)…"
    (cd "$ROOT_DIR" && cargo build --release --workspace)
fi

BIN_DEST="${DESTDIR}${PREFIX}/bin"
APP_DEST="${DESTDIR}${PREFIX}/share/applications"
PORTAL_DEST="${DESTDIR}${PREFIX}/share/xdg-desktop-portal/portals"
PORTAL_CONF_DEST="${DESTDIR}${PREFIX}/share/xdg-desktop-portal"
DOC_DEST="${DESTDIR}${PREFIX}/share/doc/feather-de"

install -d "$BIN_DEST" "$APP_DEST" "$PORTAL_DEST" "$PORTAL_CONF_DEST" "$DOC_DEST"

BINS=(
    feather-de
    feather-files
    feather-terminal
    feather-notes
    feather-preview
    feather-textedit
    feather-calc
)

echo "==> Установка бинарных файлов в $BIN_DEST…"
for bin in "${BINS[@]}"; do
    install -Dm755 "$TARGET_DIR/$bin" "$BIN_DEST/$bin"
done

# Сессионные обёртки для запуска из экранного менеджера (SDDM / GDM / greetd / TTY)
cat > "$BIN_DEST/feather-session" <<'EOF'
#!/usr/bin/env bash
set -u
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
export LIBSEAT_BACKEND="${LIBSEAT_BACKEND:-logind}"
if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" && -S "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/bus" ]]; then
    export DBUS_SESSION_BUS_ADDRESS="unix:path=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/bus"
fi
unset WAYLAND_DISPLAY DISPLAY
exec feather-de --drm --standalone "$@" >> "$LOG_DIR/drm.log" 2>&1
EOF
chmod 755 "$BIN_DEST/feather-session"

echo "==> Установка ярлыков приложений (.desktop) и конфигурации портала…"
for desktop in "$ROOT_DIR"/packaging/desktop/*.desktop; do
    install -Dm644 "$desktop" "$APP_DEST/$(basename "$desktop")"
done
install -Dm644 "$ROOT_DIR/packaging/portal/feather.portal" "$PORTAL_DEST/feather.portal"
install -Dm644 "$ROOT_DIR/packaging/portal/feather-portals.conf" "$PORTAL_CONF_DEST/featherde-portals.conf"
install -Dm644 "$ROOT_DIR/README.md" "$DOC_DEST/README.md"
install -Dm644 "$ROOT_DIR/CHANGELOG.md" "$DOC_DEST/CHANGELOG.md"
install -Dm644 "$ROOT_DIR/docs/SPEC.md" "$DOC_DEST/SPEC.md"
install -Dm644 "$ROOT_DIR/docs/ROADMAP.md" "$DOC_DEST/ROADMAP.md"

if [[ "$USER_MODE" -eq 0 ]]; then
    WSESSIONS_DEST="${DESTDIR}${PREFIX}/share/wayland-sessions"
    PAM_DEST="${DESTDIR}${SYSCONFDIR}/pam.d"
    GREETD_DEST="${DESTDIR}${SYSCONFDIR}/greetd"
    install -d "$WSESSIONS_DEST" "$PAM_DEST" "$GREETD_DEST"

    cat > "$WSESSIONS_DEST/feather-de-drm.desktop" <<'EOF'
[Desktop Entry]
Name=Пёрышко (прямой вывод DRM/KMS)
Comment=Пёрышко напрямую на видеокарте (KMS/DRM), без промежуточного композитора
Exec=feather-session
Type=Application
DesktopNames=FeatherDE
Keywords=wayland;compositor;macos;feather;rust;drm;
EOF
    chmod 644 "$WSESSIONS_DEST/feather-de-drm.desktop"

    install -Dm644 "$ROOT_DIR/session/feather-lock.pam" "$PAM_DEST/feather-lock"
    install -Dm644 "$ROOT_DIR/session/feather-greeter.toml" "$GREETD_DEST/feather-greeter.toml"
fi

echo "==> Установка Пёрышка завершена успешно."
