#!/usr/bin/env bash
# Публикация пакетов Пёрышка в APT-репозиторий (каталог ветки gh-pages → GitHub Pages).
#
#   ./packaging/publish-apt.sh КАТАЛОГ_РЕПОЗИТОРИЯ пакет.deb… [--key ID_КЛЮЧА] [--keep N]
#
# Кладёт пакеты в pool/, оставляет N последних версий (по умолчанию 3), строит
# dists/stable/main/binary-<арх>/Packages(.gz) и Release, подписывает их (InRelease и
# Release.gpg) ключом GPG и кладёт рядом публичный ключ feather-de.asc, установку одной
# командой install.sh (packaging/get.sh) и страницу с инструкцией.
# Нужны только dpkg-dev и gpg. Подключение у пользователя:
#   deb [signed-by=/etc/apt/keyrings/feather-de.asc] https://zumka1991.github.io/feather-de stable main
set -euo pipefail

REPO=""
DEBS=()
KEY=""
KEEP=3
while (( $# )); do
    case "$1" in
        --key) KEY="${2:?Укажите ключ}"; shift 2 ;;
        --keep) KEEP="${2:?Укажите число}"; shift 2 ;;
        -h|--help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) if [[ -z "$REPO" ]]; then REPO="$1"; else DEBS+=("$(readlink -f "$1")"); fi; shift ;;
    esac
done
[[ -n "$REPO" ]] || { echo "Укажите каталог репозитория (см. --help)" >&2; exit 1; }
SCRIPT_DIR="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
command -v dpkg-scanpackages >/dev/null || { echo "Нужен dpkg-dev" >&2; exit 1; }
mkdir -p "$REPO"
cd "$REPO"

POOL=pool/main/f/feather-de
mkdir -p "$POOL"
for deb in "${DEBS[@]}"; do
    [[ "$(dpkg-deb -f "$deb" Package)" == feather-de ]] || { echo "Не пакет feather-de: $deb" >&2; exit 1; }
    cp -f "$deb" "$POOL/"
done

# Последние KEEP версий для каждой архитектуры, старые — прочь (пользователи могут откатиться
# на предыдущую, а репозиторий не растёт)
for arch in $(ls "$POOL" | sed -n 's/.*_\([^_]*\)\.deb$/\1/p' | sort -u); do
    mapfile -t versions < <(for f in "$POOL"/*_"$arch".deb; do dpkg-deb -f "$f" Version; done \
        | sort -u | sort -V -r)
    for old in "${versions[@]:$KEEP}"; do
        rm -f "$POOL"/feather-de_"${old//:/%3a}"_"$arch".deb "$POOL"/feather-de_"$old"_"$arch".deb
        echo "Убрана старая версия $old ($arch)"
    done
done

ARCHES="$(ls "$POOL" | sed -n 's/.*_\([^_]*\)\.deb$/\1/p' | grep -v '^all$' | sort -u | xargs)"
DIST=dists/stable
rm -rf "$DIST"
for arch in $ARCHES; do
    dir="$DIST/main/binary-$arch"
    mkdir -p "$dir"
    dpkg-scanpackages --multiversion --arch "$arch" pool/ /dev/null 2>/dev/null > "$dir/Packages"
    gzip -9nkf "$dir/Packages"
done

# Release: даты и суммы всех индексов (пути — относительно dists/stable)
{
    echo "Origin: Feather DE"
    echo "Label: Feather DE"
    echo "Suite: stable"
    echo "Codename: stable"
    echo "Date: $(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S UTC')"
    echo "Architectures: $ARCHES"
    echo "Components: main"
    echo "Description: Пёрышко (Feather DE) — рабочий стол Wayland в стиле macOS"
    for sum in MD5Sum:md5sum SHA256:sha256sum; do
        echo "${sum%%:*}:"
        (cd "$DIST" && find main -type f -name 'Packages*' | sort | while read -r f; do
            printf ' %s %16d %s\n' "$(${sum##*:} "$f" | cut -d' ' -f1)" "$(stat -c %s "$f")" "$f"
        done)
    done
} > "$DIST/Release"

if [[ -n "$KEY" ]]; then
    gpg --batch --yes --local-user "$KEY" --clearsign -o "$DIST/InRelease" "$DIST/Release"
    gpg --batch --yes --local-user "$KEY" --armor --detach-sign -o "$DIST/Release.gpg" "$DIST/Release"
    gpg --batch --yes --armor --export "$KEY" > feather-de.asc
else
    echo "[!] Без --key репозиторий не подписан: apt откажется его использовать" >&2
fi

# curl -fsSL https://zumka1991.github.io/feather-de/install.sh | bash
install -m644 "$SCRIPT_DIR/get.sh" install.sh

# GitHub Pages отдаёт файлы как есть, без Jekyll
touch .nojekyll
cat > index.html <<'EOF'
<!doctype html>
<html lang="ru"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Пёрышко — установка</title>
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:760px;margin:40px auto;padding:0 16px;color:#1d1d1f}
pre{background:#f5f5f7;padding:12px 14px;border-radius:10px;overflow-x:auto}h2{margin-top:32px}</style></head>
<body><h1>Пёрышко (Feather DE)</h1>
<p>Рабочий стол Wayland в стиле macOS для Ubuntu 24.04 и новее (amd64).</p>
<h2>Одной командой</h2>
<p>Подходит и для Ubuntu Server без рабочего стола: ставит Пёрышко, его экран входа и всё нужное.
Запускайте обычным пользователем, пароль sudo скрипт спросит сам.</p>
<pre>curl -fsSL https://zumka1991.github.io/feather-de/install.sh | bash</pre>
<h2>Файлом .deb</h2>
<p><a href="https://github.com/Zumka1991/feather-de/releases/latest/download/feather-de_amd64.deb">Скачать feather-de_amd64.deb</a>
и открыть двойным щелчком (или <code>sudo apt install ./feather-de_amd64.deb</code>).
Пакет сам подключит этот репозиторий, как Chrome.</p>
<h2>Репозиторий вручную</h2>
<pre>sudo install -d -m 755 /etc/apt/keyrings
curl -fsSL https://zumka1991.github.io/feather-de/feather-de.asc | sudo tee /etc/apt/keyrings/feather-de.asc &gt;/dev/null
echo 'deb [signed-by=/etc/apt/keyrings/feather-de.asc] https://zumka1991.github.io/feather-de stable main' | sudo tee /etc/apt/sources.list.d/feather-de.list
sudo apt-get update &amp;&amp; sudo apt-get install feather-de</pre>
<p>Отпечаток ключа: <code>D5E6 C262 8719 EF1D 6068 5758 C7F9 44C1 485C 5FC4</code>.
Обновления приходят вместе с системой: <code>sudo apt upgrade</code>.
Исходный код: <a href="https://github.com/Zumka1991/feather-de">github.com/Zumka1991/feather-de</a>.</p>
</body></html>
EOF
echo "==> Репозиторий готов: $REPO ($(find pool -name '*.deb' | wc -l) пакетов, архитектуры: $ARCHES)"
