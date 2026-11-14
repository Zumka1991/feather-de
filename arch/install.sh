#!/usr/bin/env bash
# Установка Пёрышка (Feather DE) на Arch Linux одной командой, готовым пакетом из
# pacman-репозитория проекта:
#
#   curl -fsSL https://zumka1991.github.io/feather-de/arch/install.sh | bash
#
# Параметры (через «| bash -s -- ПАРАМЕТРЫ»): --minimal — только Пёрышко, без звука, Wi-Fi,
# Bluetooth, эмодзи и порталов. Запускать обычным пользователем: sudo скрипт попросит сам.
# Подключает репозиторий [feather-de] в /etc/pacman.conf и ключ подписи
# (D5E6 C262 8719 EF1D 6068 5758 C7F9 44C1 485C 5FC4) в pacman-key, ставит пакет и
# объясняет, как выбрать Пёрышко на экране входа. Нынешний рабочий стол и экран входа
# не трогаются: Пёрышко добавляется ещё одним сеансом. Дальше обновления — «sudo pacman -Syu».
set -euo pipefail

REPO_URL=https://zumka1991.github.io/feather-de
KEY_FINGERPRINT=D5E6C2628719EF1D60685758C7F944C1485C5FC4
PACMAN_CONF=/etc/pacman.conf
# То, что делает Центр управления живым: звук, Wi-Fi, Bluetooth, эмодзи, системные диалоги
EXTRAS=(noto-fonts-emoji pipewire pipewire-pulse libpulse networkmanager bluez bluez-utils
    xdg-desktop-portal poppler desktop-file-utils xdg-user-dirs)

step() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m[x] %s\033[0m\n' "$*" >&2; exit 1; }

# Всё в функции: bash дочитывает скрипт из curl до конца, прежде чем что-то выполнить, и
# pacman не съест остаток скрипта со стандартного ввода
main() {
    local minimal=0
    for arg in "$@"; do
        case "$arg" in
            --minimal) minimal=1 ;;
            *) die "Неизвестный параметр: $arg (есть --minimal)" ;;
        esac
    done
    [[ "$EUID" -ne 0 ]] || die "Запускайте обычным пользователем: sudo скрипт попросит сам."
    command -v pacman >/dev/null || die "Нужен pacman (Arch Linux или производная)."
    [[ "$(uname -m)" == x86_64 ]] \
        || die "Пакет есть только для x86_64; на $(uname -m) соберите из исходников (makepkg, см. README)."
    # shellcheck source=/dev/null
    . /etc/os-release
    echo "Система: ${PRETTY_NAME:-Arch Linux}"
    sudo -v || die "Нужны права sudo."

    step "Ключ подписи репозитория"
    # Своим ключом pacman заверяет чужие (--lsign-key); без него (свежий контейнер) — создать
    sudo pacman-key --list-secret-keys 2>/dev/null | grep -q '^sec' || sudo pacman-key --init
    if sudo pacman-key --list-keys "$KEY_FINGERPRINT" >/dev/null 2>&1; then
        echo "Уже добавлен."
    else
        command -v curl >/dev/null || sudo pacman -S --needed --noconfirm curl </dev/null
        curl -fsSL "$REPO_URL/feather-de.asc" | sudo pacman-key --add -
    fi
    sudo pacman-key --lsign-key "$KEY_FINGERPRINT" >/dev/null

    step "Репозиторий [feather-de]"
    if grep -q '^\[feather-de\]' "$PACMAN_CONF"; then
        echo "Уже подключён в $PACMAN_CONF."
    else
        printf '\n[feather-de]\nSigLevel = Required\nServer = %s/arch/$arch\n' "$REPO_URL" \
            | sudo tee -a "$PACMAN_CONF" >/dev/null
        echo "Добавлен в конец $PACMAN_CONF."
    fi

    step "Установка Пёрышка"
    local pkgs=(feather-de)
    (( minimal )) || pkgs+=("${EXTRAS[@]}")
    # Arch не поддерживает частичные обновления: -Syu, а не -Sy
    sudo pacman -Syu --needed --noconfirm "${pkgs[@]}" </dev/null

    if (( ! minimal )); then
        # Wi-Fi в Центре управления работает через NetworkManager. Включаем его, только если
        # сетью ещё никто не управляет, чтобы не оборвать настроенное соединение
        if ! systemctl is-enabled --quiet NetworkManager 2>/dev/null \
            && ! systemctl is-active --quiet systemd-networkd iwd dhcpcd connman 2>/dev/null; then
            sudo systemctl enable --now NetworkManager || true
        fi
        systemctl is-enabled --quiet bluetooth 2>/dev/null || sudo systemctl enable --now bluetooth 2>/dev/null || true
    fi

    step "Как запустить Пёрышко"
    local dm=""
    dm="$(basename "$(readlink -f /etc/systemd/system/display-manager.service 2>/dev/null)" .service 2>/dev/null || true)"
    echo "Ваш рабочий стол остаётся как был: Пёрышко — ещё один сеанс на экране входа."
    case "$dm" in
        sddm) echo "Экран входа SDDM: выйдите из сеанса, внизу слева выберите сеанс" ;;
        gdm) echo "Экран входа GDM: выйдите из сеанса, выберите пользователя, нажмите шестерёнку внизу справа и выберите" ;;
        lightdm) echo "Экран входа LightDM: выйдите из сеанса и в списке сеансов выберите" ;;
        ly|ly@*) echo "Экран входа ly: выйдите из сеанса, стрелками влево/вправо в строке сеанса выберите" ;;
        greetd) echo "Экран входа greetd: выберите сеанс (в tuigreet — клавиша F3)" ;;
        *) echo "Экрана входа нет: на консоли (Ctrl+Alt+F3) войдите и наберите  feather-session"
           echo "А на экране входа, если поставите его позже, выберите" ;;
    esac
    printf '    \033[1m«Пёрышко (прямой вывод DRM/KMS)»\033[0m и войдите как обычно.\n'
    echo "Попробовать в окне, не выходя из сеанса:  feather-de --windowed"
    echo "Аварийный выход: Ctrl+Alt+Backspace. Лог: ~/.local/state/feather-de/drm.log"
    echo "Обновления приходят вместе с системой:  sudo pacman -Syu"
}

main "$@"
