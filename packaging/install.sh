#!/usr/bin/env bash
# Универсальный установщик Пёрышка (Feather DE)
# Поддерживает:
#   - системную установку (PREFIX=/usr DESTDIR=/pkg ./packaging/install.sh)
#   - локальную установку в профиль пользователя (./packaging/install.sh --user)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BUNDLE=0
if [[ -d "$SCRIPT_DIR/usr/bin" ]]; then
    ROOT_DIR="$SCRIPT_DIR"
    BUNDLE=1
fi
PREFIX="${PREFIX:-/usr}"
SYSCONFDIR="${SYSCONFDIR:-/etc}"
DESTDIR="${DESTDIR:-}"
USER_MODE=0
NO_BUILD=0
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}/release"
if (( BUNDLE )); then
    TARGET_DIR="$ROOT_DIR/usr/bin"
    NO_BUILD=1
fi

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
    (cd "$ROOT_DIR" && cargo build --release --locked --workspace)
fi

# Validate all sources before writing anything to the destination.
BINS=(feather-de feather-files feather-terminal feather-notes feather-preview feather-textedit feather-calc feather-installer feather-store)
for bin in "${BINS[@]}"; do
    [[ -x "$TARGET_DIR/$bin" ]] || { echo "Нет бинарника: $TARGET_DIR/$bin" >&2; exit 1; }
done

BIN_DEST="${DESTDIR}${PREFIX}/bin"
APP_DEST="${DESTDIR}${PREFIX}/share/applications"
PORTAL_DEST="${DESTDIR}${PREFIX}/share/xdg-desktop-portal/portals"
PORTAL_CONF_DEST="${DESTDIR}${PREFIX}/share/xdg-desktop-portal"
DOC_DEST="${DESTDIR}${PREFIX}/share/doc/feather-de"
FONT_DEST="${DESTDIR}${PREFIX}/share/feather-de/fonts"
LIBEXEC_DEST="${DESTDIR}${PREFIX}/libexec/feather-de"
POLKIT_DEST="${DESTDIR}${PREFIX}/share/polkit-1/actions"

install -d "$BIN_DEST" "$APP_DEST" "$PORTAL_DEST" "$PORTAL_CONF_DEST" "$DOC_DEST" "$FONT_DEST"

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
export PATH="$(cd "$(dirname "$0")" && pwd):$PATH"
if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" && -S "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/bus" ]]; then
    export DBUS_SESSION_BUS_ADDRESS="unix:path=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/bus"
fi
# Ubuntu Server задаёт LANG=C.UTF-8: тогда программы и папки («Документы»…) говорят на языке,
# выбранном в Пёрышке, если такая локаль сгенерирована. Явно настроенную локаль не трогаем.
if [[ -z "${LC_ALL:-}" && "${LANG:-C}" =~ ^(C|C\..*|POSIX)$ ]]; then
    lang="$(sed -n 's/^language=//p' "${XDG_CONFIG_HOME:-$HOME/.config}/feather-de/settings.conf" 2>/dev/null | tail -n 1)"
    case "${lang:-ru}" in
        ru) loc=ru_RU ;; en) loc=en_US ;; es) loc=es_ES ;; de) loc=de_DE ;; fr) loc=fr_FR ;; zh) loc=zh_CN ;; *) loc= ;;
    esac
    if [[ -n "$loc" ]] && locale -a 2>/dev/null | grep -qix "$loc.utf8"; then
        export LANG="$loc.UTF-8"
        unset LANGUAGE
    fi
fi
# Домашние папки (Документы, Загрузки, Изображения…): на сервере их никто не создаёт
command -v xdg-user-dirs-update >/dev/null && xdg-user-dirs-update >/dev/null 2>&1
unset WAYLAND_DISPLAY DISPLAY
exec feather-de --drm --standalone "$@" >> "$LOG_DIR/drm.log" 2>&1
EOF
chmod 755 "$BIN_DEST/feather-session"

# Свои шрифты (Inter, JetBrains Mono, символы): программы читают их отсюда напрямую,
# интерфейс не зависит от шрифтов системы
echo "==> Установка шрифтов Пёрышка в $FONT_DEST…"
FONT_SOURCE="$ROOT_DIR/assets/fonts"
(( BUNDLE )) && FONT_SOURCE="$ROOT_DIR/usr/share/feather-de/fonts"
for font in "$FONT_SOURCE"/*; do
    install -Dm644 "$font" "$FONT_DEST/$(basename "$font")"
done

echo "==> Установка ярлыков приложений (.desktop) и конфигурации портала…"
DESKTOP_SOURCE="$ROOT_DIR/packaging/desktop"
PORTAL_SOURCE="$ROOT_DIR/packaging/portal"
DOC_SOURCE="$ROOT_DIR"
if (( BUNDLE )); then
    DESKTOP_SOURCE="$ROOT_DIR/usr/share/applications"
    PORTAL_SOURCE="$ROOT_DIR/usr/share/xdg-desktop-portal"
    DOC_SOURCE="$ROOT_DIR/usr/share/doc/feather-de"
fi
for desktop in "$DESKTOP_SOURCE"/*.desktop; do
    install -Dm644 "$desktop" "$APP_DEST/$(basename "$desktop")"
done
# Программы по умолчанию в сеансе Пёрышка: .deb открывает «Установка программ»
install -Dm644 "$DESKTOP_SOURCE/featherde-mimeapps.list" "$APP_DEST/featherde-mimeapps.list"

# Установщик пакетов: root-помощник для pkexec и действие polkit с понятным текстом запроса
echo "==> Установка помощника установщика пакетов…"
if (( BUNDLE )); then
    install -Dm755 "$ROOT_DIR/usr/libexec/feather-de/install-deb" "$LIBEXEC_DEST/install-deb"
    POLICY_SOURCE="$ROOT_DIR/usr/share/polkit-1/actions/org.featherde.install-deb.policy"
else
    install -Dm755 "$ROOT_DIR/packaging/installer/install-deb" "$LIBEXEC_DEST/install-deb"
    POLICY_SOURCE="$ROOT_DIR/packaging/installer/org.featherde.install-deb.policy.in"
fi
if [[ "$USER_MODE" -eq 0 ]]; then
    install -d "$POLKIT_DEST"
    # Путь помощника без DESTDIR: так он будет лежать в установленной системе
    sed "s|@LIBEXECDIR@|${PREFIX}/libexec|; s|/usr/libexec/feather-de/install-deb|${PREFIX}/libexec/feather-de/install-deb|" \
        "$POLICY_SOURCE" > "$POLKIT_DEST/org.featherde.install-deb.policy"
    chmod 644 "$POLKIT_DEST/org.featherde.install-deb.policy"
fi
# Индекс типов файлов (mimeinfo.cache): по нему порталы и GLib ищут, чем открыть .deb
if [[ -z "$DESTDIR" ]]; then
    if command -v update-desktop-database >/dev/null; then
        update-desktop-database -q "$APP_DEST" || true
    else
        echo "[!] Нет update-desktop-database (пакет desktop-file-utils): браузер не найдёт, чем открыть .deb" >&2
    fi
fi
if (( BUNDLE )); then
    install -Dm644 "$PORTAL_SOURCE/portals/feather.portal" "$PORTAL_DEST/feather.portal"
    install -Dm644 "$PORTAL_SOURCE/featherde-portals.conf" "$PORTAL_CONF_DEST/featherde-portals.conf"
    for doc in README.md CHANGELOG.md SPEC.md ROADMAP.md; do
        install -Dm644 "$DOC_SOURCE/$doc" "$DOC_DEST/$doc"
    done
    install -Dm644 "$ROOT_DIR/README-TEST.md" "$DOC_DEST/README-TEST.md"
else
    install -Dm644 "$PORTAL_SOURCE/feather.portal" "$PORTAL_DEST/feather.portal"
    install -Dm644 "$PORTAL_SOURCE/feather-portals.conf" "$PORTAL_CONF_DEST/featherde-portals.conf"
    install -Dm644 "$ROOT_DIR/README.md" "$DOC_DEST/README.md"
    install -Dm644 "$ROOT_DIR/CHANGELOG.md" "$DOC_DEST/CHANGELOG.md"
    install -Dm644 "$ROOT_DIR/docs/SPEC.md" "$DOC_DEST/SPEC.md"
    install -Dm644 "$ROOT_DIR/docs/ROADMAP.md" "$DOC_DEST/ROADMAP.md"
    install -Dm644 "$ROOT_DIR/packaging/README-TEST.md" "$DOC_DEST/README-TEST.md"
fi

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

    if (( BUNDLE )); then
        install -Dm644 "$ROOT_DIR/etc/pam.d/feather-lock" "$PAM_DEST/feather-lock"
        install -Dm644 "$ROOT_DIR/etc/greetd/feather-greeter.toml" "$GREETD_DEST/feather-greeter.toml"
    else
        install -Dm644 "$ROOT_DIR/session/feather-lock.pam" "$PAM_DEST/feather-lock"
        install -Dm644 "$ROOT_DIR/session/feather-greeter.toml" "$GREETD_DEST/feather-greeter.toml"
    fi
fi

echo "==> Установка Пёрышка завершена успешно."
