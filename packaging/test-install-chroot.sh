#!/usr/bin/env bash
# Проверка пакета в чистой Ubuntu без root (mmdebstrap --mode=unshare, ничего не остаётся):
#
#   ./packaging/test-install-chroot.sh dist/feather-de_X.Y.Z_amd64.deb [--suite resolute]
#
# A. .deb, поставленный файлом, ставится со всеми зависимостями и сам подключает
#    APT-репозиторий; purge убирает подключение.
# B. get.sh (установка одной командой, как через «curl | sudo bash»: setuid-sudo в пространстве
#    имён не работает) ставит этот же пакет (FEATHER_PACKAGE) и настраивает greetd, локали и сеть.
# Кэш пакетов: $FEATHER_APT_CACHE, /mnt/data/feather-os/apt-cache или ~/.cache/feather-apt-test.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
DEB=""
SUITE=resolute
while (( $# )); do
    case "$1" in
        --suite) SUITE="${2:?Укажите выпуск Ubuntu}"; shift 2 ;;
        -h|--help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) DEB="$(readlink -f "$1")"; shift ;;
    esac
done
[[ -f "$DEB" ]] || { echo "Укажите пакет .deb (см. --help)" >&2; exit 1; }
command -v mmdebstrap >/dev/null || { echo "Нужен mmdebstrap" >&2; exit 1; }
CACHE="${FEATHER_APT_CACHE:-}"
if [[ -z "$CACHE" ]]; then
    if [[ -w /mnt/data/feather-os/apt-cache ]]; then CACHE=/mnt/data/feather-os/apt-cache
    else CACHE="$HOME/.cache/feather-apt-test"; fi
fi
mkdir -p "$CACHE"
NAME="$(basename "$DEB")"
MIRROR=http://archive.ubuntu.com/ubuntu
COMPONENTS="main restricted universe multiverse"

# Проверки внутри; ошибка любой из них валит сборку
CHECKS="$(mktemp)"
trap 'rm -f "$CHECKS"' EXIT
cat > "$CHECKS" <<EOF
set -eu
# Как в обычной Ubuntu: mmdebstrap по умолчанию отключает Recommends
echo 'APT::Install-Recommends "true";' > /etc/apt/apt.conf.d/99zz-feather-test
echo "=== A. $NAME файлом, без репозитория"
apt-get install -y /tmp/$NAME >/tmp/a.log 2>&1 || { tail -30 /tmp/a.log; exit 1; }
cat /etc/apt/sources.list.d/feather-de.sources
apt-get update >/dev/null
apt-cache policy feather-de | sed -n '1,4p'
apt-cache policy feather-de | grep -q zumka1991.github.io
test -x /usr/libexec/feather-de/xwayland-satellite
test -x /usr/libexec/feather-de/setup-desktop
apt-get purge -y feather-de >/dev/null
test ! -e /etc/apt/sources.list.d/feather-de.sources
echo "purge убрал подключение репозитория"

echo "=== B. get.sh через sudo от пользователя tester"
useradd -m -s /bin/bash tester
cat /tmp/get.sh | SUDO_USER=tester FEATHER_PACKAGE=/tmp/$NAME bash >/tmp/b.log 2>&1 || { tail -40 /tmp/b.log; exit 1; }
grep -E '==>|\[x\]|\[!\]' /tmp/b.log
ls /etc/apt/sources.list.d/
test "\$(grep -rls zumka1991 /etc/apt/sources.list.d | wc -l)" = 1
grep -q 'feather-session --login' /etc/greetd/config.toml
grep -q 'user = "tester"' /etc/greetd/config.toml
systemctl is-enabled greetd.service
test "\$(systemctl get-default)" = graphical.target
locale -a | grep -qix ru_RU.utf8
test -f /etc/netplan/99-feather-network-manager.yaml
systemctl is-enabled NetworkManager.service
dpkg -s feather-de | grep -E '^(Status|Version)'
echo "=== Всё в порядке"
EOF

mmdebstrap --mode=unshare --variant=minbase --format=null \
    --include=sudo,curl,ca-certificates,gpg,systemd,systemd-sysv,locales,netplan.io \
    --setup-hook='mkdir -p "$1/var/cache/apt/archives"' \
    --setup-hook="sync-in '$CACHE' /var/cache/apt/archives" \
    --customize-hook="copy-in '$DEB' '$ROOT_DIR/packaging/get.sh' '$CHECKS' /tmp" \
    --customize-hook="chroot \"\$1\" env HOME=/root sh /tmp/$(basename "$CHECKS")" \
    --customize-hook="sync-out /var/cache/apt/archives '$CACHE'" \
    "$SUITE" /dev/null "deb $MIRROR $SUITE $COMPONENTS" "deb $MIRROR $SUITE-updates $COMPONENTS"
