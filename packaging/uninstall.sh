#!/usr/bin/env bash
# Удаление установленных файлов Пёрышка (Feather DE)
set -euo pipefail

# Messages: Russian under a Russian locale, English otherwise
case "${LC_ALL:-${LC_MESSAGES:-${LANG:-}}}" in ru*) _L=ru ;; *) _L=en ;; esac
t() { if [[ $_L == ru ]]; then printf '%s' "$1"; else printf '%s' "$2"; fi; }

PREFIX="${PREFIX:-/usr}"
SYSCONFDIR="${SYSCONFDIR:-/etc}"
DESTDIR="${DESTDIR:-}"

USER_MODE=0
if [[ "${1:-}" == "--user" ]]; then
    PREFIX="$HOME/.local"
    USER_MODE=1
fi

BIN_DEST="${DESTDIR}${PREFIX}/bin"
APP_DEST="${DESTDIR}${PREFIX}/share/applications"
PORTAL_DEST="${DESTDIR}${PREFIX}/share/xdg-desktop-portal/portals"
PORTAL_CONF_DEST="${DESTDIR}${PREFIX}/share/xdg-desktop-portal"
WSESSIONS_DEST="${DESTDIR}${PREFIX}/share/wayland-sessions"
DOC_DEST="${DESTDIR}${PREFIX}/share/doc/feather-de"

for bin in feather-de feather-files feather-terminal feather-notes feather-preview feather-textedit feather-calc feather-monitor feather-installer feather-store featherctl feather-session; do
    rm -f "$BIN_DEST/$bin"
done

for app in feather-files feather-terminal feather-notes feather-preview feather-textedit feather-calc feather-monitor feather-installer feather-store; do
    rm -f "$APP_DEST/$app.desktop"
done
rm -f "$APP_DEST/featherde-mimeapps.list" "${DESTDIR}${PREFIX}/share/polkit-1/actions/org.featherde.install-deb.policy"
rm -rf "${DESTDIR}${PREFIX}/libexec/feather-de"

rm -f "$PORTAL_DEST/feather.portal" "$PORTAL_CONF_DEST/featherde-portals.conf"
rm -f "$WSESSIONS_DEST/feather-de-drm.desktop"
rm -rf "$DOC_DEST" "${DESTDIR}${PREFIX}/share/feather-de"
if (( ! USER_MODE )); then
    rm -f "${DESTDIR}${SYSCONFDIR}/pam.d/feather-lock"
    rm -f "${DESTDIR}${SYSCONFDIR}/greetd/feather-greeter.toml"
    # install-ubuntu.sh мог настроить greetd на feather-session: без этого вход сломается.
    GREETD_CONF="${DESTDIR}${SYSCONFDIR}/greetd/config.toml"
    if [[ -f "$GREETD_CONF" ]] && grep -q feather-session "$GREETD_CONF"; then
        if [[ -f "$GREETD_CONF.before-feather" ]]; then
            mv -f "$GREETD_CONF.before-feather" "$GREETD_CONF"
            echo "$(t "==> Восстановлен прежний $GREETD_CONF." "==> The previous $GREETD_CONF was restored.")"
        else
            echo "$(t "[!] $GREETD_CONF запускает feather-session; укажите в нём другую команду." "[!] $GREETD_CONF runs feather-session; set another command in it.")" >&2
        fi
    fi
fi

echo "$(t "==> Пёрышко (Feather DE) удалено из ${DESTDIR}${PREFIX}." "==> Feather DE was removed from ${DESTDIR}${PREFIX}.")"
