#!/usr/bin/env bash
# Публикация пакетов Пёрышка в pacman-репозиторий для Arch Linux (каталог arch/ ветки gh-pages).
#
#   ./packaging/publish-arch.sh КАТАЛОГ_РЕПОЗИТОРИЯ пакет.pkg.tar.zst… [--key ID_КЛЮЧА]
#
# Раскладывает пакеты по <каталог>/<архитектура>/, подписывает их (.sig) и базу feather-de.db
# ключом GPG (тем же, что и APT-репозиторий) и кладёт рядом установку одной командой
# (packaging/get-arch.sh → install.sh). Хранится только текущая версия: pacman всё равно
# ставит самую новую, а откат — из кэша /var/cache/pacman/pkg. Нужны repo-add (pacman) и gpg.
# Подключение у пользователя (/etc/pacman.conf):
#   [feather-de]
#   SigLevel = Required
#   Server = https://zumka1991.github.io/feather-de/arch/$arch
set -euo pipefail

REPO=""
PKGS=()
KEY=""
while (( $# )); do
    case "$1" in
        --key) KEY="${2:?Укажите ключ}"; shift 2 ;;
        -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) if [[ -z "$REPO" ]]; then REPO="$1"; else PKGS+=("$(readlink -f "$1")"); fi; shift ;;
    esac
done
[[ -n "$REPO" && ${#PKGS[@]} -gt 0 ]] || { echo "Укажите каталог и пакеты (см. --help)" >&2; exit 1; }
SCRIPT_DIR="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
command -v repo-add >/dev/null || { echo "Нужен repo-add (пакет pacman)" >&2; exit 1; }
DB=feather-de

for pkg in "${PKGS[@]}"; do
    name="$(basename "$pkg")"
    [[ "$name" == feather-de-[0-9]*.pkg.tar.* && "$name" != *.sig ]] \
        || { echo "Не пакет Пёрышка: $pkg" >&2; exit 1; }
    # feather-de-0.19.6-1-x86_64.pkg.tar.zst → x86_64
    arch="${name%.pkg.tar.*}"
    arch="${arch##*-}"
    dir="$REPO/$arch"
    mkdir -p "$dir"
    # Старые версии убираем: в репозитории одна, текущая
    find "$dir" -maxdepth 1 -name 'feather-de-[0-9]*.pkg.tar.*' ! -name "$name" ! -name "$name.sig" -delete
    cp -f "$pkg" "$dir/"
    if [[ -f "$pkg.sig" ]]; then
        cp -f "$pkg.sig" "$dir/"
    elif [[ -n "$KEY" ]]; then
        gpg --batch --yes --local-user "$KEY" --detach-sign -o "$dir/$name.sig" "$dir/$name"
    fi
    sign=()
    [[ -n "$KEY" ]] && sign=(--sign --key "$KEY")
    (cd "$dir" && rm -f "$DB".db* "$DB".files* && repo-add -q "${sign[@]}" "$DB.db.tar.zst" "$name")
    # repo-add делает feather-de.db символьной ссылкой; GitHub Pages ссылки не отдаёт — копии
    for link in "$dir/$DB.db" "$dir/$DB.files" "$dir/$DB.db.sig" "$dir/$DB.files.sig"; do
        [[ -L "$link" ]] && cp --remove-destination "$(readlink -f "$link")" "$link"
    done
    echo "==> $arch: $name"
done
[[ -n "$KEY" ]] || echo "[!] Без --key репозиторий не подписан: pacman с SigLevel = Required откажется" >&2

# curl -fsSL https://zumka1991.github.io/feather-de/arch/install.sh | bash
install -m644 "$SCRIPT_DIR/get-arch.sh" "$REPO/install.sh"
echo "==> pacman-репозиторий готов: $REPO"
