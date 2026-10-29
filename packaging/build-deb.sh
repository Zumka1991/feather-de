#!/usr/bin/env bash
# Сборка пакета feather-de_<версия>_<арх>.deb для Ubuntu/Debian (без root).
#
#   ./packaging/build-deb.sh                 # cargo build --release, затем пакет в dist/
#   ./packaging/build-deb.sh --no-build      # упаковать уже собранные target/release
#   ./packaging/build-deb.sh --out КАТАЛОГ   # куда положить .deb (по умолчанию dist/)
#
# Версия: из Cargo.toml; если HEAD не помечен тегом v<версия>, к ней добавляется
# +git<дата UTC>.<коммит>: так каждая сборка новее предыдущей и apt её обновляет.
# Файлы раскладывает packaging/install.sh (как при установке из исходников), затем
# PAM-правило заменяется на стек Debian, а зависимости от библиотек считает dpkg-shlibdeps.
# В пакет входят своя сборка xwayland-satellite (в Ubuntu 26.04 его нет), setup-desktop
# (экран входа и локали для get.sh) и ключ APT-репозитория: пакет, поставленный файлом,
# сам подключает репозиторий, как Chrome, и дальше Пёрышко обновляется через apt.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
OUT_DIR="$ROOT_DIR/dist"
NO_BUILD=0
while (( $# )); do
    case "$1" in
        --no-build) NO_BUILD=1; shift ;;
        --out) OUT_DIR="${2:?Укажите каталог для --out}"; shift 2 ;;
        -h|--help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Неизвестный параметр: $1 (см. --help)" >&2; exit 1 ;;
    esac
done

for tool in dpkg-deb dpkg-shlibdeps; do
    command -v "$tool" >/dev/null || { echo "Нужна программа $tool (пакет dpkg-dev)" >&2; exit 1; }
done

BASE_VERSION="$(awk -F'"' '/^version = / { print $2; exit }' "$ROOT_DIR/Cargo.toml")"
if git -C "$ROOT_DIR" describe --exact-match --tags --match "v$BASE_VERSION" HEAD >/dev/null 2>&1; then
    VERSION="$BASE_VERSION"
else
    VERSION="$BASE_VERSION+git$(date -u +%Y%m%d%H%M).$(git -C "$ROOT_DIR" rev-parse --short HEAD)"
fi
ARCH="$(dpkg --print-architecture)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}/release"
# shellcheck source=versions.sh
. "$ROOT_DIR/packaging/versions.sh"
XWLS_BIN="${CARGO_TARGET_DIR:-$ROOT_DIR/target}/xwayland-satellite/bin/xwayland-satellite"

if (( ! NO_BUILD )); then
    echo "==> Сборка release (все программы окружения)…"
    (cd "$ROOT_DIR" && cargo build --release --locked --workspace)
    # Нужны libxcb1-dev, libxcb-cursor-dev и clang; повторный запуск с той же версией мгновенный
    echo "==> Сборка xwayland-satellite $XWLS_TAG…"
    cargo install --locked --git https://github.com/Supreeeme/xwayland-satellite --tag "$XWLS_TAG" \
        --root "$(dirname "$(dirname "$XWLS_BIN")")" xwayland-satellite
fi
[[ -x "$XWLS_BIN" ]] || { echo "Нет $XWLS_BIN: соберите без --no-build" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PKG="$WORK/pkg"

echo "==> Раскладка файлов…"
DESTDIR="$PKG" PREFIX=/usr SYSCONFDIR=/etc "$ROOT_DIR/packaging/install.sh" --no-build --target-dir "$TARGET_DIR" >/dev/null

# Конфиг greetd из репозитория — личный пример разработчика; экран входа настраивает
# дистрибутив или install-ubuntu.sh
rm -f "$PKG/etc/greetd/feather-greeter.toml"
rmdir --ignore-fail-on-non-empty "$PKG/etc/greetd"
LIBEXEC="$PKG/usr/libexec/feather-de"
install -Dm755 "$XWLS_BIN" "$LIBEXEC/xwayland-satellite"
install -Dm755 "$ROOT_DIR/packaging/setup-desktop.sh" "$LIBEXEC/setup-desktop"
install -Dm644 "$ROOT_DIR/packaging/apt/feather-de.asc" "$PKG/usr/share/keyrings/feather-de.asc"
# Экран блокировки: стек PAM Debian/Ubuntu вместо system-auth из Arch
cat > "$PKG/etc/pam.d/feather-lock" <<'EOF'
#%PAM-1.0
# Экран блокировки Пёрышка (Debian/Ubuntu)
@include common-auth
@include common-account
EOF

DOC="$PKG/usr/share/doc/feather-de"
cat > "$DOC/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: feather-de
Source: https://github.com/Zumka1991/feather-de

Files: *
Copyright: $(date -u +%Y) Zumka1991
License: none
 The project has not chosen a license for its own code yet (see README.md).

Files: usr/libexec/feather-de/xwayland-satellite
Copyright: xwayland-satellite contributors
License: MPL-2.0
 Source: https://github.com/Supreeeme/xwayland-satellite ($XWLS_TAG).
 On Debian systems the full text is in /usr/share/common-licenses/MPL-2.0.

Files: usr/share/feather-de/fonts/*
Copyright: The Inter Project Authors; JetBrains s.r.o.; Google LLC
License: OFL-1.1 or Apache-2.0
 See the *.copyright files in /usr/share/feather-de/fonts.
EOF
gzip -9n -c "$ROOT_DIR/CHANGELOG.md" > "$DOC/changelog.gz" && rm -f "$DOC/CHANGELOG.md"

echo "==> Зависимости от библиотек…"
# dpkg-shlibdeps ищет debian/control в текущем каталоге
mkdir -p "$WORK/src/debian"
printf 'Source: feather-de\n\nPackage: feather-de\nArchitecture: any\n' > "$WORK/src/debian/control"
ELF_ARGS=()
for bin in "$PKG"/usr/bin/feather-* "$LIBEXEC/xwayland-satellite"; do
    # feather-session is a shell script
    [[ "$(head -c 4 "$bin")" == $'\x7fELF' ]] && ELF_ARGS+=("-e$bin")
done
SHLIBS="$(cd "$WORK/src" && dpkg-shlibdeps -O "${ELF_ARGS[@]}" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')"
[[ -n "$SHLIBS" ]] || { echo "dpkg-shlibdeps не смог посчитать зависимости" >&2; exit 1; }

# Программы, которые окружение вызывает само; Recommends — то, без чего работает, но беднее
DEPENDS="$SHLIBS, dbus-user-session, fontconfig, pkexec, polkitd, xdg-user-dirs, xdg-utils, locales, desktop-file-utils, shared-mime-info, libarchive-tools"
RECOMMENDS="breeze-cursor-theme, libfuse2t64 | libfuse2, xwayland, xdg-desktop-portal, xdg-desktop-portal-gtk, pipewire-pulse, wireplumber, pulseaudio-utils, network-manager, bluez, poppler-utils, ffmpeg, ddcutil, fonts-noto-core, fonts-noto-color-emoji, fonts-noto-cjk, fonts-dejavu-core"
INSTALLED_KB="$(du -sk --exclude=DEBIAN "$PKG" | cut -f1)"

mkdir -p "$PKG/DEBIAN"
cat > "$PKG/DEBIAN/control" <<EOF
Package: feather-de
Version: $VERSION
Architecture: $ARCH
Maintainer: Zumka1991 <244128845+Zumka1991@users.noreply.github.com>
Installed-Size: $INSTALLED_KB
Depends: $DEPENDS
Recommends: $RECOMMENDS
Section: x11
Priority: optional
Homepage: https://github.com/Zumka1991/feather-de
Description: Пёрышко (Feather DE) — рабочий стол Wayland в стиле macOS
 Быстрое и лёгкое окружение рабочего стола на Rust: композитор Wayland с
 Доком, Launchpad, Центром управления и строкой меню, а также свои Файлы,
 Терминал, Заметки, Просмотр, TextEdit, Калькулятор и установщик пакетов.
 Запускается сеансом «Пёрышко» из экрана входа или командой feather-session.
EOF
echo "/etc/pam.d/feather-lock" > "$PKG/DEBIAN/conffiles"
cat > "$PKG/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
# Новые ярлыки и тип .deb → «Установка программ»
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
# Как Chrome: пакет, поставленный файлом, сам подключает свой репозиторий, и дальше Пёрышко
# обновляется через apt. Уже подключённый (образ Пёрышко OS, инструкция с сайта, get.sh) не
# трогаем. Отключить обновления: строка «Enabled: no» в этом файле (он не перезаписывается).
SOURCES=/etc/apt/sources.list.d/feather-de.sources
if [ "$1" = configure ] && [ ! -e "$SOURCES" ] \
    && ! grep -qs 'zumka1991\.github\.io/feather-de' /etc/apt/sources.list \
        /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; then
    cat > "$SOURCES" <<'SRC'
# Подключено пакетом feather-de: новые версии Пёрышка приходят с «sudo apt upgrade».
# Отключить: Enabled: no
Types: deb
URIs: https://zumka1991.github.io/feather-de
Suites: stable
Components: main
Signed-By: /usr/share/keyrings/feather-de.asc
SRC
fi
EOF
cat > "$PKG/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
# Репозиторий, который подключил postinst, убираем вместе с настройками (purge)
SOURCES=/etc/apt/sources.list.d/feather-de.sources
if [ "$1" = purge ] && grep -qs '^# Подключено пакетом feather-de' "$SOURCES"; then
    rm -f "$SOURCES"
fi
EOF
chmod 755 "$PKG/DEBIAN/postinst" "$PKG/DEBIAN/postrm"

mkdir -p "$OUT_DIR"
DEB="$OUT_DIR/feather-de_${VERSION}_${ARCH}.deb"
# Права как у системных пакетов, что бы ни было в umask; владелец — root, без fakeroot
chmod -R u+rwX,go+rX,go-w "$PKG"
dpkg-deb --root-owner-group -Zxz --build "$PKG" "$DEB" >/dev/null
echo "==> Готово: $DEB ($(du -h "$DEB" | cut -f1))"
