#!/usr/bin/env bash
# Удаление установленных файлов Пёрышка (Feather DE)
set -euo pipefail

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

for bin in feather-de feather-files feather-terminal feather-notes feather-preview feather-textedit feather-calc feather-session; do
    rm -f "$BIN_DEST/$bin"
done

for app in feather-files feather-terminal feather-notes feather-preview feather-textedit feather-calc; do
    rm -f "$APP_DEST/$app.desktop"
done

rm -f "$PORTAL_DEST/feather.portal" "$PORTAL_CONF_DEST/featherde-portals.conf"
rm -f "$WSESSIONS_DEST/feather-de-drm.desktop"
rm -rf "$DOC_DEST"
if (( ! USER_MODE )); then
    rm -f "${DESTDIR}${SYSCONFDIR}/pam.d/feather-lock"
    rm -f "${DESTDIR}${SYSCONFDIR}/greetd/feather-greeter.toml"
fi

echo "==> Пёрышко (Feather DE) удалено из ${DESTDIR}${PREFIX}."
