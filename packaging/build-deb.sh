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

if (( ! NO_BUILD )); then
    echo "==> Сборка release (все программы окружения)…"
    (cd "$ROOT_DIR" && cargo build --release --locked --workspace)
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PKG="$WORK/pkg"

echo "==> Раскладка файлов…"
DESTDIR="$PKG" PREFIX=/usr SYSCONFDIR=/etc "$ROOT_DIR/packaging/install.sh" --no-build --target-dir "$TARGET_DIR" >/dev/null

# Конфиг greetd из репозитория — личный пример разработчика; экран входа настраивает
# дистрибутив или install-ubuntu.sh
rm -f "$PKG/etc/greetd/feather-greeter.toml"
rmdir --ignore-fail-on-non-empty "$PKG/etc/greetd"
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
for bin in "$PKG"/usr/bin/feather-*; do
    # feather-session is a shell script
    [[ "$(head -c 4 "$bin")" == $'\x7fELF' ]] && ELF_ARGS+=("-e$bin")
done
SHLIBS="$(cd "$WORK/src" && dpkg-shlibdeps -O "${ELF_ARGS[@]}" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')"
[[ -n "$SHLIBS" ]] || { echo "dpkg-shlibdeps не смог посчитать зависимости" >&2; exit 1; }

# Программы, которые окружение вызывает само; Recommends — то, без чего работает, но беднее
DEPENDS="$SHLIBS, dbus-user-session, fontconfig, pkexec, polkitd, xdg-user-dirs, xdg-utils, locales, desktop-file-utils, shared-mime-info, libarchive-tools"
RECOMMENDS="breeze-cursor-theme, xwayland, xdg-desktop-portal, xdg-desktop-portal-gtk, pipewire-pulse, wireplumber, pulseaudio-utils, network-manager, bluez, poppler-utils, ffmpeg, ddcutil, fonts-noto-core, fonts-noto-color-emoji, fonts-noto-cjk, fonts-dejavu-core"
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
for script in postinst postrm; do
    cat > "$PKG/DEBIAN/$script" <<'EOF'
#!/bin/sh
set -e
# Новые ярлыки и тип .deb → «Установка программ»
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
EOF
    chmod 755 "$PKG/DEBIAN/$script"
done

mkdir -p "$OUT_DIR"
DEB="$OUT_DIR/feather-de_${VERSION}_${ARCH}.deb"
# Права как у системных пакетов, что бы ни было в umask; владелец — root, без fakeroot
chmod -R u+rwX,go+rX,go-w "$PKG"
dpkg-deb --root-owner-group -Zxz --build "$PKG" "$DEB" >/dev/null
echo "==> Готово: $DEB ($(du -h "$DEB" | cut -f1))"
