#!/usr/bin/env bash
# Installs Feather (Feather DE, "Пёрышко") on Ubuntu with one command, from the APT repository:
#
#   curl -fsSL https://zumka1991.github.io/feather-de/install.sh | bash
#
# On Arch Linux the same command runs the Arch installer (arch/install.sh, get-arch.sh).
#
# Options (pass them with "| bash -s -- OPTIONS"): --minimal installs only Feather, without
# the feather-desktop app set; --tuigreet, --greetd, --no-greetd work as in setup-desktop.
# Run it as a regular user: the script asks for sudo itself (with "| sudo bash" the login
# screen would be set up for whoever called sudo).
# It adds the repository (key D5E6 C262 8719 EF1D 6068 5758 C7F9 44C1 485C 5FC4), installs the
# feather-desktop metapackage (Feather and the programs of Ubuntu Desktop: office, video,
# music, the store, printing, codecs, drivers), then runs setup-desktop: locales and, if there
# is no login screen (Ubuntu Server), the Feather login screen. After that, updates come with
# the usual "sudo apt upgrade". Building from source: packaging/install-ubuntu.sh.
# Messages are in Russian under a Russian locale and in English otherwise.
set -euo pipefail

# Messages: Russian under a Russian locale, English otherwise
case "${LC_ALL:-${LC_MESSAGES:-${LANG:-}}}" in ru*) _L=ru ;; *) _L=en ;; esac
t() { if [[ $_L == ru ]]; then printf '%s' "$1"; else printf '%s' "$2"; fi; }

REPO_URL=https://zumka1991.github.io/feather-de
KEY_FINGERPRINT=D5E6C2628719EF1D60685758C7F944C1485C5FC4
KEYRING=/etc/apt/keyrings/feather-de.asc
SOURCES=/etc/apt/sources.list.d/feather-de.sources

step() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m[x] %s\033[0m\n' "$*" >&2; exit 1; }

# Всё в функции: bash дочитывает скрипт из curl до конца, прежде чем что-то выполнить, и
# apt не съест остаток скрипта со стандартного ввода
main() {
    local setup_args=() package=feather-desktop
    for arg in "$@"; do
        case "$arg" in
            --minimal) package=feather-de ;;
            --tuigreet|--greetd|--no-greetd) setup_args+=("$arg") ;;
            *) die "$(t "Неизвестный параметр: $arg (есть --minimal, --tuigreet, --greetd, --no-greetd)" "Unknown option: $arg (available: --minimal, --tuigreet, --greetd, --no-greetd)")" ;;
        esac
    done

    # Arch Linux и производные (apt нет, есть pacman): там свой репозиторий и свой установщик
    # (packaging/get-arch.sh), параметры те же. FEATHER_ARCH_INSTALLER=путь — проверка
    # неопубликованного установщика.
    if ! command -v apt-get >/dev/null && command -v pacman >/dev/null; then
        local arch_installer rc=0
        arch_installer="$(mktemp)"
        if [[ -n "${FEATHER_ARCH_INSTALLER:-}" ]]; then
            cp "$FEATHER_ARCH_INSTALLER" "$arch_installer"
        else
            curl -fsSL "$REPO_URL/arch/install.sh" -o "$arch_installer" \
                || { rm -f "$arch_installer"; die "$(t "Не удалось скачать установщик для Arch Linux: $REPO_URL/arch/install.sh" "Could not download the Arch Linux installer: $REPO_URL/arch/install.sh")"; }
        fi
        bash "$arch_installer" "$@" || rc=$?
        rm -f "$arch_installer"
        exit "$rc"
    fi

    # Экран входа Пёрышка открывается для этого пользователя
    local login_user="$USER"
    if [[ "$EUID" -eq 0 ]]; then
        login_user="${SUDO_USER:-}"
        [[ -n "$login_user" && "$login_user" != root ]] \
            || die "$(t "Запускайте обычным пользователем: экран входа настраивается на вас, sudo скрипт попросит сам." "Run it as a regular user: the login screen is set up for you, and the script asks for sudo itself.")"
    fi
    command -v apt-get >/dev/null || die "$(t "Нужен apt (Ubuntu 24.04 или новее)." "apt is required (Ubuntu 24.04 or newer).")"
    # shellcheck source=/dev/null
    . /etc/os-release
    [[ "${ID:-}" == ubuntu || " ${ID_LIKE:-} " == *" ubuntu "* ]] \
        || die "$(t "Система ${PRETTY_NAME:-неизвестна}: пакет собран для Ubuntu." "System ${PRETTY_NAME:-unknown}: the package is built for Ubuntu.")"
    [[ "$(dpkg --print-architecture)" == amd64 ]] \
        || die "$(t "Пакет есть только для amd64; на $(dpkg --print-architecture) соберите из исходников (packaging/install-ubuntu.sh)." "The package is only built for amd64; on $(dpkg --print-architecture) build from source (packaging/install-ubuntu.sh).")"
    echo "$(t "Система: ${PRETTY_NAME:-Ubuntu}" "System: ${PRETTY_NAME:-Ubuntu}")"
    sudo -v || die "$(t "Нужны права sudo." "sudo rights are required.")"
    export DEBIAN_FRONTEND=noninteractive

    step "$(t "Репозиторий Пёрышка" "Feather repository")"
    if grep -qs 'zumka1991\.github\.io/feather-de' /etc/apt/sources.list \
        /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; then
        echo "$(t "Уже подключён." "Already added.")"
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
                || { rm -f "$key"; die "$(t "Ключ репозитория не совпадает с ожидаемым ($KEY_FINGERPRINT)." "The repository key does not match the expected one ($KEY_FINGERPRINT).")"; }
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
        echo "$(t "Подключён: $SOURCES" "Added: $SOURCES")"
    fi

    step "$(t "Списки пакетов" "Package lists")"
    sudo apt-get update </dev/null
    # Сторонний .deb с устаревшей зависимостью ломает любой apt install. Сами ничего не
    # удаляем: «apt --fix-broken install» снёс бы ту программу.
    if ! sudo apt-get check >/dev/null 2>&1 </dev/null; then
        sudo apt-get check </dev/null || true
        die "$(t "apt сообщает о неудовлетворённых зависимостях уже установленных пакетов (см. выше). Почините их и запустите снова." "apt reports unmet dependencies of already installed packages (see above). Fix them and run again.")"
    fi

    step "$(t "Установка Пёрышка" "Installing Feather")"
    # FEATHER_PACKAGE="пути/к/пакетам.deb" — проверка неопубликованной сборки
    local packages
    read -ra packages <<<"${FEATHER_PACKAGE:-$package}"
    sudo apt-get install -y "${packages[@]}" </dev/null

    sudo /usr/libexec/feather-de/setup-desktop --user "$login_user" "${setup_args[@]}" </dev/null
    echo "$(t "  Удаление:                    sudo apt purge feather-de feather-desktop" "  Uninstall:                   sudo apt purge feather-de feather-desktop")"
}

main "$@"
