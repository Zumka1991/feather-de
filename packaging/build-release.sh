#!/usr/bin/env bash
# Сборка тестового архива Пёрышка (системные библиотеки не включены).
# Использование:
#   ./packaging/build-release.sh                        -> создаёт архив в dist/
#   ./packaging/build-release.sh --no-build             -> упаковывает готовые release-бинарники
#   ./packaging/build-release.sh --stage-only /tmp/dir  -> раскладывает дерево пакета в каталог
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
VERSION="$(awk -F'"' '/^version = / { print $2; exit }' "$ROOT_DIR/Cargo.toml")"
ARCH="$(uname -m)"
STAGE_ONLY=""
NO_BUILD=0

while (( $# )); do
    case "$1" in
        --stage-only) STAGE_ONLY="${2:?Укажите каталог для --stage-only}"; shift 2 ;;
        --no-build) NO_BUILD=1; shift ;;
        *) echo "Использование: $0 [--no-build] [--stage-only КАТАЛОГ]" >&2; exit 1 ;;
    esac
done

if [[ -n "$STAGE_ONLY" ]]; then
    if [[ -e "$STAGE_ONLY" && -n "$(ls -A "$STAGE_ONLY")" ]]; then
        echo "Каталог стейджинга должен быть пустым: $STAGE_ONLY" >&2
        exit 1
    fi
    mkdir -p "$STAGE_ONLY"
    DESTDIR="$STAGE_ONLY" PREFIX="/usr" SYSCONFDIR="/etc" "$ROOT_DIR/packaging/install.sh" --no-build
    echo "==> Стейджинг релиза $VERSION готов в $STAGE_ONLY"
    exit 0
fi

if (( ! NO_BUILD )); then
    echo "==> Сборка релизных бинарников Пёрышка v${VERSION} (${ARCH})…"
    (cd "$ROOT_DIR" && cargo build --release --locked --workspace)
fi

DIST_DIR="$ROOT_DIR/dist"
PKG_NAME="feather-de-${VERSION}-arch-linux-${ARCH}"
STAGE_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGE_DIR"' EXIT

mkdir -p "$DIST_DIR" "$STAGE_DIR/$PKG_NAME"
DESTDIR="$STAGE_DIR/$PKG_NAME" PREFIX="/usr" SYSCONFDIR="/etc" "$ROOT_DIR/packaging/install.sh" --no-build
install -Dm755 "$ROOT_DIR/packaging/install.sh" "$STAGE_DIR/$PKG_NAME/install.sh"
install -Dm755 "$ROOT_DIR/packaging/uninstall.sh" "$STAGE_DIR/$PKG_NAME/uninstall.sh"
install -Dm755 "$ROOT_DIR/packaging/run-demo.sh" "$STAGE_DIR/$PKG_NAME/run-demo.sh"
install -Dm755 "$ROOT_DIR/packaging/check-dependencies.sh" "$STAGE_DIR/$PKG_NAME/check-dependencies.sh"
install -Dm644 "$ROOT_DIR/packaging/README-TEST.md" "$STAGE_DIR/$PKG_NAME/README-TEST.md"
{
    echo "version=$VERSION"
    echo "arch=$ARCH"
    echo "built_at=$(date -u +%FT%TZ)"
    echo "commit=$(git -C "$ROOT_DIR" rev-parse HEAD)"
    echo "working_tree_changes=$(git -C "$ROOT_DIR" status --porcelain --untracked-files=normal | wc -l)"
    echo "build_libc=$(getconf GNU_LIBC_VERSION)"
    echo "rustc=$(rustc --version)"
    echo "required_glibc=$(readelf --version-info "$STAGE_DIR/$PKG_NAME"/usr/bin/feather-{de,files,terminal,notes,preview,textedit,calc,installer,store} | grep -oE 'GLIBC_[0-9.]+' | sort -Vu | tail -1)"
} > "$STAGE_DIR/$PKG_NAME/BUILD-INFO.txt"
(
    cd "$STAGE_DIR/$PKG_NAME"
    find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS
)

ARCHIVE="$DIST_DIR/${PKG_NAME}.tar.gz"
tar -C "$STAGE_DIR" -czf "$ARCHIVE" "$PKG_NAME"
(cd "$DIST_DIR" && sha256sum "$(basename "$ARCHIVE")" > "$(basename "$ARCHIVE").sha256")

echo "==> Готов релизный архив: $ARCHIVE"
cat "$ARCHIVE.sha256"
