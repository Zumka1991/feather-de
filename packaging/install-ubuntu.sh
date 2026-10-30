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
#   5. packaging/setup-desktop.sh: локали; если экрана входа нет (сервер) — greetd с экраном
#      входа самого Пёрышка, графическая загрузка и сеть под NetworkManager.
# Без сборки, готовым пакетом: curl -fsSL https://zumka1991.github.io/feather-de/install.sh | bash
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
            sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'
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
    # bsdtar: «Сжать» и распаковка архивов в Файлах (zip, tar.*, 7z); unar — RAR, включая
    # RAR5 и многотомные (bsdtar на них зависает или сбивается)
    libarchive-tools unar
    fonts-noto-core fonts-noto-mono fonts-noto-color-emoji fonts-noto-cjk fonts-dejavu-core
    breeze-cursor-theme polkitd
)
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
    libfuse2t64                               # AppImage монтируются сами (без него — распаковкой)
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
# shellcheck source=versions.sh
. "$ROOT_DIR/packaging/versions.sh"
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

# --- Экран входа, локали и сеть ---------------------------------------------------------------

SETUP_ARGS=(--user "$USER")
[[ "$GREETD" == yes ]] && SETUP_ARGS+=(--greetd)
[[ "$GREETD" == no ]] && SETUP_ARGS+=(--no-greetd)
(( TUIGREET )) && SETUP_ARGS+=(--tuigreet)
sudo "$ROOT_DIR/packaging/setup-desktop.sh" "${SETUP_ARGS[@]}"
echo "  Удаление:                    sudo $ROOT_DIR/packaging/uninstall.sh"
