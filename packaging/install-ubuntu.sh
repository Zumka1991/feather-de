#!/usr/bin/env bash
# Установка Пёрышка (Feather DE) на Ubuntu из исходников одной командой.
#
# Запуск из корня репозитория (или из packaging/), обычным пользователем, не через sudo:
#   ./packaging/install-ubuntu.sh               # зависимости, Rust, сборка, установка в /usr
#   ./packaging/install-ubuntu.sh --no-optional # без Wi-Fi/звука/порталов и прочих дополнений
#   ./packaging/install-ubuntu.sh --greetd      # поставить экран входа greetd даже при наличии GDM
#   ./packaging/install-ubuntu.sh --no-greetd   # не ставить экран входа (запуск: feather-session с TTY)
#   ./packaging/install-ubuntu.sh --tuigreet    # текстовый экран входа tuigreet (для нескольких пользователей)
#
# Подходит и для Ubuntu Server / минимальной системы без рабочего стола.
# Что делает:
#   1. ставит через apt заголовки для сборки и пакеты времени выполнения;
#   2. ставит Rust через rustup в ~/.cargo (apt-версия Rust обычно старее нужной);
#   3. собирает весь workspace в release;
#   4. ставит программы в /usr, сеанс «Пёрышко (прямой вывод DRM/KMS)» и PAM-правило
#      экрана блокировки в формате Debian/Ubuntu (common-auth вместо system-auth из Arch);
#   5. если экрана входа нет (сервер), ставит greetd с экраном входа самого Пёрышка (как в macOS:
#      заставка, аватар, пароль; выход из сеанса возвращает на него), включает графическую загрузку
#      и передаёт сеть NetworkManager (для Wi-Fi в Центре управления).
# Если стоит GNOME/GDM, он не трогается: Пёрышко появляется в его списке сеансов.
# Удаление: sudo ./packaging/uninstall.sh
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
OPTIONAL=1
GREETD=auto
TUIGREET=0

for arg in "$@"; do
    case "$arg" in
        --no-optional) OPTIONAL=0 ;;
        --greetd) GREETD=yes ;;
        --no-greetd) GREETD=no ;;
        --tuigreet) TUIGREET=1 ;;
        -h|--help)
            sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "Неизвестный параметр: $arg (см. --help)" >&2
            exit 1
            ;;
    esac
done

step() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33m[!] %s\033[0m\n' "$*" >&2; }
die() { printf '\033[1;31m[x] %s\033[0m\n' "$*" >&2; exit 1; }

# --- Проверки -------------------------------------------------------------------------------

[[ "$EUID" -ne 0 ]] || die "Запускайте обычным пользователем: Rust ставится в ваш профиль, sudo скрипт попросит сам."
[[ -f "$ROOT_DIR/Cargo.toml" && -x "$ROOT_DIR/packaging/install.sh" ]] \
    || die "Не найден репозиторий Пёрышка в $ROOT_DIR."
command -v apt-get >/dev/null || die "Нужен apt (Ubuntu или Debian)."

# shellcheck source=/dev/null
. /etc/os-release
if [[ "${ID:-}" != ubuntu && " ${ID_LIKE:-} " != *" ubuntu "* ]]; then
    warn "Система: ${PRETTY_NAME:-неизвестно}. Скрипт рассчитан на Ubuntu; продолжаю на свой страх."
else
    echo "Система: ${PRETTY_NAME:-Ubuntu}"
fi

case "$(uname -m)" in
    x86_64|aarch64) ;;
    *) warn "Архитектура $(uname -m) не проверялась." ;;
esac

# Экран входа уже есть (GDM, SDDM, LightDM…), если systemd знает display-manager.service.
# greetd, уже настроенный на Пёрышко прошлым запуском, — свой: его конфиг обновляем.
HAS_DM=0
[[ -e /etc/systemd/system/display-manager.service ]] && HAS_DM=1
if [[ "$(basename "$(readlink -f /etc/systemd/system/display-manager.service 2>/dev/null)")" == greetd.service ]] \
    && grep -qs feather-session /etc/greetd/config.toml; then
    HAS_DM=0
fi
if [[ "$GREETD" == auto ]]; then
    if (( HAS_DM )); then GREETD=no; else GREETD=yes; fi
fi
if (( HAS_DM )); then
    echo "Экран входа: $(basename "$(readlink -f /etc/systemd/system/display-manager.service)" .service)"
else
    echo "Экран входа не найден (система без рабочего стола)."
fi

# --- Пакеты ---------------------------------------------------------------------------------

# Сборка: компилятор, pkg-config и заголовки для drm/gbm/libinput/libseat/udev/xkbcommon.
BUILD_PKGS=(
    build-essential pkg-config curl ca-certificates
    libdrm-dev libgbm-dev libinput-dev libseat-dev libudev-dev libxkbcommon-dev
    libwayland-dev libegl-dev libgles-dev libpam0g-dev
)
# Запуск: Mesa (EGL/GLES и DRI-драйверы), D-Bus, PAM, курсоры, домашние папки и шрифты.
# Свои шрифты (Inter, JetBrains Mono) и иконки Пёрышко носит с собой; системные Noto нужны
# Firefox и другим программам, а также для иероглифов и эмодзи. На Ubuntu Server нет ничего из
# того, что обычно приносит рабочий стол: без xdg-user-dirs нет «Документов» и «Загрузок».
RUNTIME_PKGS=(
    libegl1 libgles2 libgl1-mesa-dri libgbm1 libinput10 libseat1 libxkbcommon0
    libpam0g fontconfig dbus dbus-user-session xdg-utils xdg-user-dirs locales
    # update-desktop-database: без индекса типов файлов порталы (браузер из snap) не находят,
    # чем открыть .deb и другие файлы
    desktop-file-utils shared-mime-info
    # bsdtar: «Сжать» и распаковка архивов в Файлах (zip, tar.*, 7z, rar)
    libarchive-tools
    fonts-noto-core fonts-noto-mono fonts-noto-color-emoji fonts-noto-cjk fonts-dejavu-core
    breeze-cursor-theme polkitd
    # Ubuntu вырезает переводы из пакетов в языковые пакеты: без них «Документы» и
    # «Загрузки» называются Documents/Downloads даже в русском сеансе (как и меню GTK-программ)
    language-pack-ru-base language-pack-de-base language-pack-es-base language-pack-fr-base
    language-pack-zh-hans-base
)
# Экран входа для системы без рабочего стола: greetd запускает Пёрышко с его экраном входа.
GREETER_PKGS=(greetd)
(( TUIGREET )) && GREETER_PKGS+=(tuigreet)
# Необязательные: каждая строка включает свою часть Центра управления или программы.
OPTIONAL_PKGS=(
    xwayland-satellite                        # X11-программы (Steam, Wine, старые приложения)
    xwayland                                  # X-сервер, который запускает xwayland-satellite
    xdg-desktop-portal xdg-desktop-portal-gtk # диалоги файлов, захват экрана
    pipewire pipewire-pulse wireplumber pulseaudio-utils # звук и pactl
    network-manager                           # Wi-Fi
    bluez                                     # Bluetooth
    poppler-utils                             # pdftoppm для PDF в Просмотре
    ffmpeg                                    # запись экрана
    ddcutil                                   # яркость внешних мониторов
)

# Ставим только то, что есть в репозиториях этой версии Ubuntu: имя пакета может отсутствовать
# (например, xwayland-satellite появился не во всех выпусках).
available() {
    local pkg
    for pkg in "$@"; do
        if apt-cache show "$pkg" >/dev/null 2>&1; then
            printf '%s\n' "$pkg"
        else
            warn "Пакета $pkg нет в репозиториях, пропускаю."
        fi
    done
}

step "Обновление списков пакетов"
sudo apt-get update
# Сторонний .deb с устаревшей зависимостью (например policykit-1, которого нет в 26.04) ломает
# любой apt install. Сами ничего не удаляем: `apt --fix-broken install` снёс бы ту программу.
if ! sudo apt-get check >/dev/null 2>&1; then
    sudo apt-get check || true
    die "apt сообщает о неудовлетворённых зависимостях уже установленных пакетов (см. выше). Почините их и запустите скрипт снова; осторожно: «apt --fix-broken install» может удалить программу с устаревшей зависимостью."
fi

step "Установка зависимостей сборки и запуска"
mapfile -t REQUIRED < <(available "${BUILD_PKGS[@]}" "${RUNTIME_PKGS[@]}")
sudo apt-get install -y --no-install-recommends "${REQUIRED[@]}"

if (( OPTIONAL )); then
    step "Установка дополнительных пакетов (звук, сеть, порталы, X11)"
    mapfile -t EXTRA < <(available "${OPTIONAL_PKGS[@]}")
    if (( ${#EXTRA[@]} )); then
        sudo apt-get install -y "${EXTRA[@]}"
    fi
fi

if [[ "$GREETD" == yes ]]; then
    step "Установка экрана входа greetd + tuigreet"
    mapfile -t GREETER < <(available "${GREETER_PKGS[@]}")
    (( ${#GREETER[@]} == ${#GREETER_PKGS[@]} )) || die "В репозиториях нет ${GREETER_PKGS[*]}; запустите с --no-greetd."
    # Не даём пакету запустить greetd прямо сейчас: он переключил бы консоль посреди установки,
    # а конфигурация Пёрышка ещё не записана. Запустится после перезагрузки.
    # Чужой policy-rc.d (бывает в контейнерах) сохраняем и возвращаем.
    POLICY_RC=/usr/sbin/policy-rc.d
    restore_policy_rc() {
        sudo rm -f "$POLICY_RC"
        if [[ -e "$POLICY_RC.feather-bak" ]]; then sudo mv -f "$POLICY_RC.feather-bak" "$POLICY_RC"; fi
    }
    if [[ -e "$POLICY_RC" ]]; then sudo mv -f "$POLICY_RC" "$POLICY_RC.feather-bak"; fi
    trap restore_policy_rc EXIT
    printf '#!/bin/sh\nexit 101\n' | sudo tee "$POLICY_RC" >/dev/null
    sudo chmod 755 "$POLICY_RC"
    sudo apt-get install -y --no-install-recommends "${GREETER[@]}"
    restore_policy_rc
    trap - EXIT
fi

# Ubuntu Server знает только C.UTF-8: генерируем локали языков Пёрышка, чтобы сеанс
# (feather-session) говорил на выбранном языке, а домашние папки назывались по-русски.
# Системную локаль (LANG в /etc/default/locale) не меняем.
step "Локали языков интерфейса"
for loc in ru_RU en_US es_ES de_DE fr_FR zh_CN; do
    if ! locale -a 2>/dev/null | grep -qix "$loc.utf8"; then
        sudo sed -i "s/^# *\($loc.UTF-8 UTF-8\)/\1/" /etc/locale.gen
        grep -qx "$loc.UTF-8 UTF-8" /etc/locale.gen || echo "$loc.UTF-8 UTF-8" | sudo tee -a /etc/locale.gen >/dev/null
        NEED_LOCALE_GEN=1
    fi
done
if [[ -n "${NEED_LOCALE_GEN:-}" ]]; then
    sudo locale-gen
fi
fc-cache -f >/dev/null 2>&1 || true

# --- Rust -----------------------------------------------------------------------------------

step "Rust toolchain"
export PATH="$HOME/.cargo/bin:$PATH"
if command -v rustup >/dev/null; then
    rustup toolchain install stable --profile minimal
    rustup default stable >/dev/null
else
    if command -v cargo >/dev/null; then
        warn "Найден cargo не из rustup ($(command -v cargo)); ставлю rustup, он будет первым в PATH."
    fi
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain stable
    # shellcheck source=/dev/null
    . "$HOME/.cargo/env"
fi
echo "$(rustc --version), $(cargo --version)"

# xwayland-satellite (X11-программы) есть не во всех выпусках Ubuntu, например его нет в 26.04:
# тогда собираем из исходников проекта.
XWLS_TAG=v0.8.3
if (( OPTIONAL )) && ! command -v xwayland-satellite >/dev/null; then
    step "Сборка xwayland-satellite $XWLS_TAG (для X11-программ)"
    mapfile -t XWLS_DEPS < <(available xwayland libxcb1-dev libxcb-cursor-dev libclang-dev clang)
    sudo apt-get install -y --no-install-recommends "${XWLS_DEPS[@]}"
    XWLS_ROOT="${CARGO_TARGET_DIR:-$ROOT_DIR/target}/xwayland-satellite"
    if cargo install --locked --git https://github.com/Supreeeme/xwayland-satellite \
        --tag "$XWLS_TAG" --root "$XWLS_ROOT" xwayland-satellite; then
        sudo install -Dm755 "$XWLS_ROOT/bin/xwayland-satellite" /usr/local/bin/xwayland-satellite
    else
        warn "xwayland-satellite не собрался: Пёрышко будет работать, но без X11-программ."
    fi
fi

# --- Сборка и установка ---------------------------------------------------------------------

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
step "Сборка Пёрышка (release, это займёт несколько минут)"
(cd "$ROOT_DIR" && cargo build --release --locked --workspace --target-dir "$TARGET_DIR")

step "Установка в /usr"
sudo env PREFIX=/usr SYSCONFDIR=/etc \
    "$ROOT_DIR/packaging/install.sh" --no-build --target-dir "$TARGET_DIR/release"

# Правило из репозитория ссылается на system-auth (Arch); в Ubuntu его нет, и экран блокировки
# не смог бы проверить пароль. Заменяем на штатный стек Debian/Ubuntu.
sudo tee /etc/pam.d/feather-lock >/dev/null <<'EOF'
#%PAM-1.0
# Экран блокировки Пёрышка (Debian/Ubuntu)
@include common-auth
@include common-account
EOF
sudo chmod 644 /etc/pam.d/feather-lock

# --- Экран входа и сеть для системы без рабочего стола ---------------------------------------

if [[ "$GREETD" == yes ]]; then
    step "Настройка greetd: вход в Пёрышко"
    GREETER_USER=_greetd
    id -u "$GREETER_USER" >/dev/null 2>&1 || GREETER_USER=greeter
    id -u "$GREETER_USER" >/dev/null 2>&1 || die "Не найден пользователь экрана входа (_greetd/greeter)."
    if [[ -f /etc/greetd/config.toml ]] && ! grep -q feather-session /etc/greetd/config.toml; then
        sudo cp /etc/greetd/config.toml /etc/greetd/config.toml.before-feather
    fi
    if (( TUIGREET )); then
        # tuigreet: имя и пароль; F2 — другая команда, F3 — другой сеанс из wayland-sessions.
        GREETER_CMD="tuigreet --time --remember --asterisks --greeting 'Пёрышко' --cmd feather-session"
    else
        # Экран входа самого Пёрышка: greetd сразу запускает его от имени этого пользователя,
        # рабочий стол открывается только после проверки пароля через PAM (feather-lock), без
        # перезапуска видеокарты. После выхода из сеанса greetd снова показывает этот экран.
        GREETER_CMD="feather-session --login"
        GREETER_USER="$USER"
    fi
    sudo tee /etc/greetd/config.toml >/dev/null <<EOF
# Установлено packaging/install-ubuntu.sh (Пёрышко). Прежний файл: config.toml.before-feather
[terminal]
vt = 7

[default_session]
command = "$GREETER_CMD"
user = "$GREETER_USER"
EOF
    sudo chmod 644 /etc/greetd/config.toml
    sudo systemctl enable greetd.service
    sudo systemctl set-default graphical.target
fi

# На Ubuntu Server сетью управляет systemd-networkd через netplan, и NetworkManager не видит
# ни Wi-Fi, ни проводную сеть. Как в Ubuntu Desktop, отдаём netplan-рендеринг NetworkManager.
# Применится после перезагрузки: netplan apply сейчас мог бы оборвать SSH.
NETPLAN_NM=/etc/netplan/99-feather-network-manager.yaml
if [[ "$GREETD" == yes ]] && (( ! HAS_DM )) && command -v NetworkManager >/dev/null \
    && command -v netplan >/dev/null && ! sudo grep -rqs 'renderer: *NetworkManager' /etc/netplan; then
    step "Передача сети NetworkManager (после перезагрузки)"
    printf 'network:\n  version: 2\n  renderer: NetworkManager\n' | sudo tee "$NETPLAN_NM" >/dev/null
    sudo chmod 600 "$NETPLAN_NM"
    if sudo netplan get >/dev/null; then
        sudo systemctl enable NetworkManager.service
        # networkd больше ничего не ведёт: его ожидание сети задерживало бы загрузку на 2 минуты.
        sudo systemctl disable systemd-networkd-wait-online.service 2>/dev/null || true
    else
        warn "netplan не принял настройку; сеть оставлена как была."
        sudo rm -f "$NETPLAN_NM"
    fi
fi

# --- Подсказки ------------------------------------------------------------------------------

if [[ -d /sys/module/nvidia ]]; then
    modeset="$(cat /sys/module/nvidia_drm/parameters/modeset 2>/dev/null || echo N)"
    if [[ "$modeset" != Y ]]; then
        warn "Проприетарный драйвер NVIDIA без modeset: Пёрышко не получит экран."
        warn "Включите: echo 'options nvidia-drm modeset=1' | sudo tee /etc/modprobe.d/nvidia-drm.conf"
        warn "затем sudo update-initramfs -u и перезагрузка."
    fi
fi

step "Готово"
if [[ "$GREETD" == yes ]]; then
    cat <<EOF
Перезагрузитесь: sudo reboot
После загрузки появится экран входа Пёрышка; введите пароль. Выход из сеанса
(меню Пёрышка → «Выйти из Пёрышка…») возвращает на этот экран.
EOF
    (( TUIGREET )) || echo "Экран входа открывается для $USER; для нескольких пользователей: --tuigreet."
    (( HAS_DM )) && warn "greetd поставлен рядом с прежним экраном входа; включён будет только один (systemctl status display-manager)."
elif (( HAS_DM )); then
    cat <<EOF
Выйдите из сеанса. На экране входа выберите пользователя, нажмите шестерёнку
в правом нижнем углу и выберите «Пёрышко (прямой вывод DRM/KMS)».
Прежний рабочий стол остался на экране входа как запасной сеанс.
EOF
else
    cat <<EOF
Экран входа не ставился. Войдите в текстовую консоль и выполните: feather-session
EOF
fi
cat <<EOF

  Аварийный выход из Пёрышка:  Ctrl+Alt+Backspace
  Консоль:                     Ctrl+Alt+F3
  Лог сеанса:                  ~/.local/state/feather-de/drm.log
  Удаление:                    sudo $ROOT_DIR/packaging/uninstall.sh
EOF
