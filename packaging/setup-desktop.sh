#!/usr/bin/env bash
# Настройка Ubuntu (в том числе Server без рабочего стола) под Пёрышко; запускается от root.
# В пакете — /usr/libexec/feather-de/setup-desktop; его вызывают get.sh (установка одной
# командой) и install-ubuntu.sh (сборка из исходников).
#
#   setup-desktop --user ИМЯ               # экран входа Пёрышка для ИМЯ, если другого нет
#   setup-desktop --user ИМЯ --tuigreet    # текстовый экран входа (для нескольких пользователей)
#   setup-desktop --user ИМЯ --greetd      # greetd даже рядом с GDM/SDDM
#   setup-desktop --user ИМЯ --no-greetd   # экран входа не трогать
#
# Что делает: языковые пакеты и локали языков интерфейса; если экрана входа нет — greetd с
# экраном входа Пёрышка, графическая загрузка и сеть под NetworkManager (Wi-Fi в Центре
# управления). Если стоит GNOME/GDM, он не трогается: Пёрышко появляется в его списке сеансов.
set -euo pipefail

LOGIN_USER=""
GREETD=auto
TUIGREET=0
while (( $# )); do
    case "$1" in
        --user) LOGIN_USER="${2:?Укажите пользователя}"; shift 2 ;;
        --greetd) GREETD=yes; shift ;;
        --no-greetd) GREETD=no; shift ;;
        --tuigreet) TUIGREET=1; shift ;;
        -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Неизвестный параметр: $1 (см. --help)" >&2; exit 1 ;;
    esac
done

step() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33m[!] %s\033[0m\n' "$*" >&2; }
die() { printf '\033[1;31m[x] %s\033[0m\n' "$*" >&2; exit 1; }

[[ "$EUID" -eq 0 ]] || die "setup-desktop запускается от root (через sudo)."
[[ -n "$LOGIN_USER" ]] && id -u "$LOGIN_USER" >/dev/null 2>&1 || die "Укажите существующего пользователя: --user ИМЯ."
export DEBIAN_FRONTEND=noninteractive

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

# Ubuntu вырезает переводы из пакетов в языковые пакеты: без них «Документы» и «Загрузки»
# называются Documents/Downloads даже в русском сеансе (как и меню GTK-программ).
step "Языковые пакеты и локали интерфейса"
mapfile -t LANGPACKS < <(available language-pack-ru-base language-pack-de-base \
    language-pack-es-base language-pack-fr-base language-pack-zh-hans-base)
if (( ${#LANGPACKS[@]} )); then
    apt-get install -y --no-install-recommends "${LANGPACKS[@]}"
fi
# Ubuntu Server знает только C.UTF-8: генерируем локали языков Пёрышка, чтобы сеанс
# (feather-session) говорил на выбранном языке. Системную локаль (LANG) не меняем.
NEED_LOCALE_GEN=0
LOCALES="$(locale -a 2>/dev/null || true)"
for loc in ru_RU en_US es_ES de_DE fr_FR zh_CN; do
    if ! grep -qix "$loc.utf8" <<<"$LOCALES"; then
        sed -i "s/^# *\($loc.UTF-8 UTF-8\)/\1/" /etc/locale.gen
        grep -qx "$loc.UTF-8 UTF-8" /etc/locale.gen || echo "$loc.UTF-8 UTF-8" >> /etc/locale.gen
        NEED_LOCALE_GEN=1
    fi
done
(( NEED_LOCALE_GEN )) && locale-gen
fc-cache -f >/dev/null 2>&1 || true

if [[ "$GREETD" == yes ]]; then
    step "Экран входа greetd"
    GREETER_PKGS=(greetd)
    (( TUIGREET )) && GREETER_PKGS+=(tuigreet)
    mapfile -t GREETER < <(available "${GREETER_PKGS[@]}")
    (( ${#GREETER[@]} == ${#GREETER_PKGS[@]} )) || die "В репозиториях нет ${GREETER_PKGS[*]}; запустите с --no-greetd."
    # Не даём пакету запустить greetd прямо сейчас: он переключил бы консоль посреди установки,
    # а конфигурация Пёрышка ещё не записана. Запустится после перезагрузки.
    # Чужой policy-rc.d (бывает в контейнерах) сохраняем и возвращаем.
    POLICY_RC=/usr/sbin/policy-rc.d
    restore_policy_rc() {
        rm -f "$POLICY_RC"
        if [[ -e "$POLICY_RC.feather-bak" ]]; then mv -f "$POLICY_RC.feather-bak" "$POLICY_RC"; fi
    }
    if [[ -e "$POLICY_RC" ]]; then mv -f "$POLICY_RC" "$POLICY_RC.feather-bak"; fi
    trap restore_policy_rc EXIT
    printf '#!/bin/sh\nexit 101\n' > "$POLICY_RC"
    chmod 755 "$POLICY_RC"
    apt-get install -y --no-install-recommends "${GREETER[@]}"
    restore_policy_rc
    trap - EXIT

    GREETER_USER=_greetd
    id -u "$GREETER_USER" >/dev/null 2>&1 || GREETER_USER=greeter
    id -u "$GREETER_USER" >/dev/null 2>&1 || die "Не найден пользователь экрана входа (_greetd/greeter)."
    if [[ -f /etc/greetd/config.toml ]] && ! grep -q feather-session /etc/greetd/config.toml; then
        cp /etc/greetd/config.toml /etc/greetd/config.toml.before-feather
    fi
    if (( TUIGREET )); then
        # tuigreet: имя и пароль; F2 — другая команда, F3 — другой сеанс из wayland-sessions.
        GREETER_CMD="tuigreet --time --remember --asterisks --greeting 'Пёрышко' --cmd feather-session"
    else
        # Экран входа самого Пёрышка: greetd сразу запускает его от имени этого пользователя,
        # рабочий стол открывается только после проверки пароля через PAM (feather-lock), без
        # перезапуска видеокарты. После выхода из сеанса greetd снова показывает этот экран.
        GREETER_CMD="feather-session --login"
        GREETER_USER="$LOGIN_USER"
    fi
    cat > /etc/greetd/config.toml <<EOF
# Установлено setup-desktop (Пёрышко). Прежний файл: config.toml.before-feather
[terminal]
vt = 7

[default_session]
command = "$GREETER_CMD"
user = "$GREETER_USER"
EOF
    chmod 644 /etc/greetd/config.toml
    systemctl enable greetd.service
    systemctl set-default graphical.target
fi

# На Ubuntu Server сетью управляет systemd-networkd через netplan, и NetworkManager не видит
# ни Wi-Fi, ни проводную сеть. Как в Ubuntu Desktop, отдаём netplan-рендеринг NetworkManager.
# Применится после перезагрузки: netplan apply сейчас мог бы оборвать SSH.
NETPLAN_NM=/etc/netplan/99-feather-network-manager.yaml
if [[ "$GREETD" == yes ]] && (( ! HAS_DM )) && command -v NetworkManager >/dev/null \
    && command -v netplan >/dev/null && ! grep -rqs 'renderer: *NetworkManager' /etc/netplan; then
    step "Передача сети NetworkManager (после перезагрузки)"
    printf 'network:\n  version: 2\n  renderer: NetworkManager\n' > "$NETPLAN_NM"
    chmod 600 "$NETPLAN_NM"
    if netplan get >/dev/null; then
        systemctl enable NetworkManager.service
        # networkd больше ничего не ведёт: его ожидание сети задерживало бы загрузку на 2 минуты.
        systemctl disable systemd-networkd-wait-online.service 2>/dev/null || true
    else
        warn "netplan не принял настройку; сеть оставлена как была."
        rm -f "$NETPLAN_NM"
    fi
fi

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
    (( TUIGREET )) || echo "Экран входа открывается для $LOGIN_USER; для нескольких пользователей: --tuigreet."
    (( HAS_DM )) && warn "greetd поставлен рядом с прежним экраном входа; включён будет только один (systemctl status display-manager)."
elif (( HAS_DM )); then
    cat <<EOF
Выйдите из сеанса. На экране входа выберите пользователя, нажмите шестерёнку
в правом нижнем углу и выберите «Пёрышко (прямой вывод DRM/KMS)».
Прежний рабочий стол остался на экране входа как запасной сеанс.
EOF
else
    echo "Экран входа не ставился. Войдите в текстовую консоль и выполните: feather-session"
fi
cat <<EOF

  Аварийный выход из Пёрышка:  Ctrl+Alt+Backspace
  Консоль:                     Ctrl+Alt+F3
  Лог сеанса:                  ~/.local/state/feather-de/drm.log
EOF
