#!/usr/bin/env bash
# Сборка самодостаточного релизного пакета Пёрышка (Feather DE)
# Использование:
#   ./packaging/build-release.sh                        -> создаёт архив в dist/
#   ./packaging/build-release.sh --stage-only /tmp/dir  -> раскладывает дерево пакета в каталог
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
VERSION="$(awk -F'"' '/^version = / { print $2; exit }' "$ROOT_DIR/Cargo.toml")"
ARCH="$(uname -m)"
STAGE_ONLY=""

if [[ "${1:-}" == "--stage-only" ]]; then
    STAGE_ONLY="${2:?Укажите каталог для --stage-only}"
fi

chmod +x "$ROOT_DIR/packaging/install.sh" "$ROOT_DIR/packaging/uninstall.sh"

if [[ -n "$STAGE_ONLY" ]]; then
    rm -rf "$STAGE_ONLY"
    mkdir -p "$STAGE_ONLY"
    DESTDIR="$STAGE_ONLY" PREFIX="/usr" SYSCONFDIR="/etc" "$ROOT_DIR/packaging/install.sh" --no-build
    echo "==> Стейджинг релиза $VERSION готов в $STAGE_ONLY"
    exit 0
fi

echo "==> Сборка релизных бинарников Пёрышка v${VERSION} (${ARCH})…"
(cd "$ROOT_DIR" && cargo build --release --workspace)

DIST_DIR="$ROOT_DIR/dist"
PKG_NAME="feather-de-${VERSION}-linux-${ARCH}"
STAGE_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGE_DIR"' EXIT

mkdir -p "$DIST_DIR" "$STAGE_DIR/$PKG_NAME"
DESTDIR="$STAGE_DIR/$PKG_NAME" PREFIX="/usr" SYSCONFDIR="/etc" "$ROOT_DIR/packaging/install.sh" --no-build
install -Dm755 "$ROOT_DIR/packaging/install.sh" "$STAGE_DIR/$PKG_NAME/install.sh"
install -Dm755 "$ROOT_DIR/packaging/uninstall.sh" "$STAGE_DIR/$PKG_NAME/uninstall.sh"
install -Dm644 "$ROOT_DIR/packaging/PKGBUILD" "$STAGE_DIR/$PKG_NAME/PKGBUILD"

ARCHIVE="$DIST_DIR/${PKG_NAME}.tar.gz"
tar -C "$STAGE_DIR" -czf "$ARCHIVE" "$PKG_NAME"
(cd "$DIST_DIR" && sha256sum "$(basename "$ARCHIVE")" > "$(basename "$ARCHIVE").sha256")

echo "==> Готов релизный архив: $ARCHIVE"
cat "$ARCHIVE.sha256"
