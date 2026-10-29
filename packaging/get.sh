#!/usr/bin/env bash
# Установка Пёрышка (Feather DE) на Ubuntu одной командой, готовым пакетом из APT-репозитория:
#
#   curl -fsSL https://zumka1991.github.io/feather-de/install.sh | bash
#
# Параметры (через «| bash -s -- ПАРАМЕТРЫ»): --tuigreet, --greetd, --no-greetd — как у
# setup-desktop. Запускать обычным пользователем: sudo скрипт попросит сам (через «| sudo bash»
# экран входа настраивается на того, кто вызвал sudo).
# Подключает репозиторий (ключ D5E6 C262 8719 EF1D 6068 5758 C7F9 44C1 485C 5FC4), ставит
# пакет feather-de с рекомендуемыми (звук, сеть, порталы, X11, шрифты), затем setup-desktop:
# локали и, если экрана входа нет (Ubuntu Server), экран входа Пёрышка. Дальше обновления —
# обычным «sudo apt upgrade». Сборка из исходников: packaging/install-ubuntu.sh.
set -euo pipefail

REPO_URL=https://zumka1991.github.io/feather-de
KEY_FINGERPRINT=D5E6C2628719EF1D60685758C7F944C1485C5FC4
KEYRING=/etc/apt/keyrings/feather-de.asc
SOURCES=/etc/apt/sources.list.d/feather-de.sources

step() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m[x] %s\033[0m\n' "$*" >&2; exit 1; }

# Всё в функции: bash дочитывает скрипт из curl до конца, прежде чем что-то выполнить, и
# apt не съест остаток скрипта со стандартного ввода
main() {
    local setup_args=()
    for arg in "$@"; do
        case "$arg" in
            --tuigreet|--greetd|--no-greetd) setup_args+=("$arg") ;;
            *) die "Неизвестный параметр: $arg (есть --tuigreet, --greetd, --no-greetd)" ;;
        esac
    done

    # Экран входа Пёрышка открывается для этого пользователя
    local login_user="$USER"
    if [[ "$EUID" -eq 0 ]]; then
        login_user="${SUDO_USER:-}"
        [[ -n "$login_user" && "$login_user" != root ]] \
            || die "Запускайте обычным пользователем: экран входа настраивается на вас, sudo скрипт попросит сам."
    fi
    command -v apt-get >/dev/null || die "Нужен apt (Ubuntu 24.04 или новее)."
    # shellcheck source=/dev/null
    . /etc/os-release
    [[ "${ID:-}" == ubuntu || " ${ID_LIKE:-} " == *" ubuntu "* ]] \
        || die "Система ${PRETTY_NAME:-неизвестна}: пакет собран для Ubuntu."
    [[ "$(dpkg --print-architecture)" == amd64 ]] \
        || die "Пакет есть только для amd64; на $(dpkg --print-architecture) соберите из исходников (packaging/install-ubuntu.sh)."
    echo "Система: ${PRETTY_NAME:-Ubuntu}"
    sudo -v || die "Нужны права sudo."
    export DEBIAN_FRONTEND=noninteractive

    step "Репозиторий Пёрышка"
    if grep -qs 'zumka1991\.github\.io/feather-de' /etc/apt/sources.list \
        /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; then
        echo "Уже подключён."
    else
        command -v curl >/dev/null || sudo apt-get install -y curl ca-certificates </dev/null
        local key
        key="$(mktemp)"
        curl -fsSL "$REPO_URL/feather-de.asc" -o "$key"
        # Ключ сверяем с отпечатком, зашитым в скрипт
        if command -v gpg >/dev/null; then
            local fingerprints gnupg
            gnupg="$(mktemp -d)"
            fingerprints="$(GNUPGHOME="$gnupg" gpg --show-keys --with-colons "$key" 2>/dev/null || true)"
            rm -rf "$gnupg"
            [[ "$fingerprints" == *"fpr:::::::::$KEY_FINGERPRINT:"* ]] \
                || { rm -f "$key"; die "Ключ репозитория не совпадает с ожидаемым ($KEY_FINGERPRINT)."; }
        fi
        sudo install -Dm644 "$key" "$KEYRING"
        rm -f "$key"
        sudo tee "$SOURCES" >/dev/null <<EOF
# Пёрышко: новые версии приходят с «sudo apt upgrade». Отключить: Enabled: no
Types: deb
URIs: $REPO_URL
Suites: stable
Components: main
Signed-By: $KEYRING
EOF
        echo "Подключён: $SOURCES"
    fi

    step "Списки пакетов"
    sudo apt-get update </dev/null
    # Сторонний .deb с устаревшей зависимостью ломает любой apt install. Сами ничего не
    # удаляем: «apt --fix-broken install» снёс бы ту программу.
    if ! sudo apt-get check >/dev/null 2>&1 </dev/null; then
        sudo apt-get check </dev/null || true
        die "apt сообщает о неудовлетворённых зависимостях уже установленных пакетов (см. выше). Почините их и запустите снова."
    fi

    step "Установка Пёрышка"
    # FEATHER_PACKAGE=путь/к/пакету.deb — проверка неопубликованной сборки
    sudo apt-get install -y "${FEATHER_PACKAGE:-feather-de}" </dev/null

    sudo /usr/libexec/feather-de/setup-desktop --user "$login_user" "${setup_args[@]}" </dev/null
    echo "  Удаление:                    sudo apt purge feather-de"
}

main "$@"
