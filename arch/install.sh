#!/usr/bin/env bash
# Установка Пёрышка (Feather DE) на Arch Linux одной командой, готовым пакетом из
# pacman-репозитория проекта:
#
#   curl -fsSL https://zumka1991.github.io/feather-de/install.sh | bash
#
# (общая команда сама запускает этот установщик; прямой адрес — …/feather-de/arch/install.sh).
# Параметры (через «| bash -s -- ПАРАМЕТРЫ»): --minimal — только Пёрышко, без набора программ,
# звука, Wi-Fi и экрана входа; --tuigreet — текстовый экран входа (для нескольких
# пользователей); --greetd — экран входа Пёрышка даже рядом с GDM/SDDM; --no-greetd — экран
# входа не трогать. Запускать обычным пользователем: sudo скрипт попросит сам.
# Подключает репозиторий [feather-de] в /etc/pacman.conf и ключ подписи
# (D5E6 C262 8719 EF1D 6068 5758 C7F9 44C1 485C 5FC4) в pacman-key, ставит Пёрышко и набор
# программ рабочего стола (как feather-desktop на Ubuntu: браузер, офис, видео, музыка, печать,
# кодеки), генерирует локали и, если экрана входа нет (голый Arch), включает экран входа
# Пёрышка. Уже стоящий рабочий стол и его экран входа не трогаются: Пёрышко добавляется ещё
# одним сеансом. Дальше обновления — «sudo pacman -Syu».
set -euo pipefail

# Messages: Russian under a Russian locale, English otherwise
case "${LC_ALL:-${LC_MESSAGES:-${LANG:-}}}" in ru*) _L=ru ;; *) _L=en ;; esac
t() { if [[ $_L == ru ]]; then printf '%s' "$1"; else printf '%s' "$2"; fi; }

REPO_URL=https://zumka1991.github.io/feather-de
KEY_FINGERPRINT=D5E6C2628719EF1D60685758C7F944C1485C5FC4
PACMAN_CONF=/etc/pacman.conf
# То, что делает Центр управления живым: звук, Wi-Fi, Bluetooth, эмодзи, системные диалоги
EXTRAS=(noto-fonts-emoji pipewire pipewire-pulse wireplumber libpulse networkmanager bluez
    bluez-utils xdg-desktop-portal xdg-desktop-portal-gtk poppler desktop-file-utils xdg-user-dirs)
# Набор программ и служб — то же, что packaging/desktop-set.list даёт на Ubuntu. То, что
# Пёрышко умеет само (Файлы, Терминал, TextEdit, Просмотр, Калькулятор, Заметки, архиватор),
# не дублируется. Каждую программу можно удалить через pacman -Rns.
DESKTOP_SET=(
    # Службы рабочего стола
    gnome-keyring                 # хранилище паролей (Chrome, VS Code, Telegram)
    gvfs gvfs-mtp gvfs-smb        # телефоны (MTP), сетевые папки, корзина в GTK-программах
    udisks2                       # флешки и диски без root
    upower                        # батарея
    power-profiles-daemon         # режимы питания
    fwupd                         # обновления прошивок
    avahi                         # принтеры и устройства в сети
    cups system-config-printer    # печать
    simple-scan                   # сканер
    flatpak                       # программы Flathub в «Магазине» Пёрышка
    # Мультимедиа и шрифты для чужих программ
    gst-libav gst-plugins-good gst-plugins-bad gst-plugin-pipewire
    qt6-wayland qt5-wayland       # Qt-программы (Telegram) в родном Wayland
    ttf-liberation                # метрики Times/Arial/Courier для документов и сайтов
    noto-fonts-cjk                # иероглифы
    hunspell hunspell-en_us       # проверка орфографии
    # Утилиты
    zip unzip 7zip
    exfatprogs ntfs-3g            # флешки exFAT и NTFS
    fuse2                         # AppImage
    # Программы
    firefox firefox-i18n-ru       # браузер
    showtime                      # видео
    rhythmbox                     # музыка
    snapshot                      # камера
    gnome-disk-utility            # диски и образы
    baobab                        # что занимает место
    gnome-characters              # символы и эмодзи
    transmission-gtk              # торренты
    libreoffice-fresh libreoffice-fresh-ru   # офис
)

step() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33m[!] %s\033[0m\n' "$*" >&2; }
die() { printf '\033[1;31m[x] %s\033[0m\n' "$*" >&2; exit 1; }

# Имена пакетов, которые есть в подключённых репозиториях; про остальные — предупреждение.
# Производные Arch (Manjaro, CachyOS) могут называть что-то иначе: это не повод сорвать установку.
available() {
    local pkg
    for pkg in "$@"; do
        if pacman -Si "$pkg" >/dev/null 2>&1; then
            printf '%s\n' "$pkg"
        else
            warn "$(t "Пакета $pkg нет в репозиториях, пропускаю." "Package $pkg is not in the repositories, skipping.")"
        fi
    done
}

# Локали языков интерфейса Пёрышка: сеанс (feather-session) говорит на выбранном языке, только
# если его локаль сгенерирована. Системную локаль (LANG) не меняем.
setup_locales() {
    local loc need=0 have
    have="$(locale -a 2>/dev/null || true)"
    for loc in ru_RU en_US es_ES de_DE fr_FR zh_CN; do
        if ! grep -qix "$loc.utf8" <<<"$have"; then
            sudo sed -i "s/^# *\($loc\.UTF-8 UTF-8\)/\1/" /etc/locale.gen
            grep -qx "$loc.UTF-8 UTF-8" /etc/locale.gen \
                || echo "$loc.UTF-8 UTF-8" | sudo tee -a /etc/locale.gen >/dev/null
            need=1
        fi
    done
    if (( need )); then
        sudo locale-gen
    else
        echo "$(t "Уже сгенерированы." "Already generated.")"
    fi
}

# Экран входа greetd: $1 — пользователь, $2 — 1 для tuigreet
setup_greetd() {
    local login_user="$1" tuigreet="$2" greeter_pkgs=(greetd) found
    (( tuigreet )) && greeter_pkgs+=(greetd-tuigreet)
    mapfile -t found < <(available "${greeter_pkgs[@]}")
    (( ${#found[@]} == ${#greeter_pkgs[@]} )) || die "$(t "В репозиториях нет ${greeter_pkgs[*]}; запустите с --no-greetd." "${greeter_pkgs[*]} is not in the repositories; run with --no-greetd.")"
    sudo pacman -S --needed --noconfirm "${found[@]}" </dev/null

    local greeter_cmd greeter_user=greeter
    if (( tuigreet )); then
        # tuigreet: имя и пароль; F2 — другая команда, F3 — другой сеанс из wayland-sessions
        id -u "$greeter_user" >/dev/null 2>&1 || die "$(t "Не найден пользователь экрана входа (greeter)." "The login screen user (greeter) was not found.")"
        greeter_cmd="tuigreet --time --remember --asterisks --greeting 'Пёрышко' --cmd feather-session"
    else
        # Экран входа самого Пёрышка: greetd сразу запускает его от имени этого пользователя,
        # рабочий стол открывается только после проверки пароля через PAM (feather-lock), без
        # перезапуска видеокарты. После выхода из сеанса greetd снова показывает этот экран.
        greeter_cmd="feather-session --login"
        greeter_user="$login_user"
    fi
    if [[ -f /etc/greetd/config.toml ]] && ! grep -q feather-session /etc/greetd/config.toml; then
        sudo cp /etc/greetd/config.toml /etc/greetd/config.toml.before-feather
    fi
    sudo tee /etc/greetd/config.toml >/dev/null <<EOF
# Установлено установщиком Пёрышка. Прежний файл: config.toml.before-feather
[terminal]
vt = 7

[default_session]
command = "$greeter_cmd"
user = "$greeter_user"
EOF
    sudo chmod 644 /etc/greetd/config.toml
    # --force: рядом с прежним экраном входа (--greetd) заменяет ссылку display-manager.service.
    # Не --now: greetd переключил бы консоль посреди установки, запустится после перезагрузки
    sudo systemctl enable --force greetd.service
    sudo systemctl set-default graphical.target
}

# Всё в функции: bash дочитывает скрипт из curl до конца, прежде чем что-то выполнить, и
# pacman не съест остаток скрипта со стандартного ввода
main() {
    local minimal=0 greetd=auto tuigreet=0
    for arg in "$@"; do
        case "$arg" in
            --minimal) minimal=1 ;;
            --greetd) greetd=yes ;;
            --no-greetd) greetd=no ;;
            --tuigreet) tuigreet=1 ;;
            *) die "$(t "Неизвестный параметр: $arg (есть --minimal, --tuigreet, --greetd, --no-greetd)" "Unknown option: $arg (available: --minimal, --tuigreet, --greetd, --no-greetd)")" ;;
        esac
    done
    [[ "$EUID" -ne 0 ]] || die "$(t "Запускайте обычным пользователем: экран входа настраивается на вас, sudo скрипт попросит сам." "Run it as a regular user: the login screen is set up for you, and the script asks for sudo itself.")"
    command -v pacman >/dev/null || die "$(t "Нужен pacman (Arch Linux или производная)." "pacman is required (Arch Linux or a derivative).")"
    [[ "$(uname -m)" == x86_64 ]] \
        || die "$(t "Пакет есть только для x86_64; на $(uname -m) соберите из исходников (makepkg, см. README)." "The package is only built for x86_64; on $(uname -m) build from source (makepkg, see the README).")"
    # shellcheck source=/dev/null
    . /etc/os-release
    echo "$(t "Система: ${PRETTY_NAME:-Arch Linux}" "System: ${PRETTY_NAME:-Arch Linux}")"
    sudo -v || die "$(t "Нужны права sudo." "sudo rights are required.")"
    local login_user
    login_user="$(id -un)"

    # Экран входа уже есть (GDM, SDDM, LightDM…), если systemd знает display-manager.service.
    # greetd, уже настроенный на Пёрышко прошлым запуском, — свой: его конфиг обновляем.
    local dm="" has_dm=0
    if [[ -e /etc/systemd/system/display-manager.service ]]; then
        has_dm=1
        dm="$(basename "$(readlink -f /etc/systemd/system/display-manager.service)" .service)"
        if [[ "$dm" == greetd ]] && grep -qs feather-session /etc/greetd/config.toml; then
            has_dm=0
        fi
    fi
    if [[ "$greetd" == auto ]]; then
        # --minimal — только пакет; --tuigreet — явная просьба об экране входа
        if (( has_dm )) || (( minimal && ! tuigreet )); then greetd=no; else greetd=yes; fi
    fi

    step "$(t "Ключ подписи репозитория" "Repository signing key")"
    # Своим ключом pacman заверяет чужие (--lsign-key); без него (свежий контейнер) — создать
    sudo pacman-key --list-secret-keys 2>/dev/null | grep -q '^sec' || sudo pacman-key --init
    if sudo pacman-key --list-keys "$KEY_FINGERPRINT" >/dev/null 2>&1; then
        echo "$(t "Уже добавлен." "Already added.")"
    else
        command -v curl >/dev/null || sudo pacman -S --needed --noconfirm curl </dev/null
        curl -fsSL "$REPO_URL/feather-de.asc" | sudo pacman-key --add -
    fi
    sudo pacman-key --lsign-key "$KEY_FINGERPRINT" >/dev/null

    step "$(t "Репозиторий [feather-de]" "Repository [feather-de]")"
    if grep -q '^\[feather-de\]' "$PACMAN_CONF"; then
        echo "$(t "Уже подключён в $PACMAN_CONF." "Already enabled in $PACMAN_CONF.")"
    else
        printf '\n[feather-de]\nSigLevel = Required\nServer = %s/arch/$arch\n' "$REPO_URL" \
            | sudo tee -a "$PACMAN_CONF" >/dev/null
        echo "$(t "Добавлен в конец $PACMAN_CONF." "Added to the end of $PACMAN_CONF.")"
    fi

    step "$(t "Установка Пёрышка" "Installing Feather")"
    # Arch не поддерживает частичные обновления: -Syu, а не -Sy
    sudo pacman -Syu --needed --noconfirm feather-de </dev/null

    if (( ! minimal )); then
        step "$(t "Набор программ рабочего стола" "Desktop app set")"
        local set_pkgs
        mapfile -t set_pkgs < <(available "${EXTRAS[@]}" "${DESKTOP_SET[@]}")
        sudo pacman -S --needed --noconfirm "${set_pkgs[@]}" </dev/null

        # Wi-Fi в Центре управления работает через NetworkManager. Включаем его, только если
        # сетью ещё никто не управляет, чтобы не оборвать настроенное соединение
        if ! systemctl is-enabled --quiet NetworkManager 2>/dev/null \
            && ! systemctl is-active --quiet systemd-networkd iwd dhcpcd connman 2>/dev/null; then
            sudo systemctl enable --now NetworkManager || true
        fi
        # На Arch пакеты свои службы не включают: Bluetooth, печать и поиск принтеров в сети
        local unit
        for unit in bluetooth.service cups.socket avahi-daemon.service; do
            systemctl is-enabled --quiet "$unit" 2>/dev/null || sudo systemctl enable --now "$unit" 2>/dev/null || true
        done

        step "$(t "Локали интерфейса" "Interface locales")"
        setup_locales
    fi

    if [[ "$greetd" == yes ]]; then
        step "$(t "Экран входа greetd" "The greetd login screen")"
        setup_greetd "$login_user" "$tuigreet"
    fi

    if [[ -d /sys/module/nvidia ]]; then
        local modeset
        modeset="$(cat /sys/module/nvidia_drm/parameters/modeset 2>/dev/null || echo N)"
        if [[ "$modeset" != Y ]]; then
            warn "$(t "Проприетарный драйвер NVIDIA без modeset: Пёрышко не получит экран." "Proprietary NVIDIA driver without modeset: Feather will not get a display.")"
            warn "$(t "Включите: echo 'options nvidia-drm modeset=1' | sudo tee /etc/modprobe.d/nvidia-drm.conf" "Enable it: echo 'options nvidia-drm modeset=1' | sudo tee /etc/modprobe.d/nvidia-drm.conf")"
            warn "$(t "затем sudo mkinitcpio -P и перезагрузка." "then run sudo mkinitcpio -P and reboot.")"
        fi
    fi

    step "$(t "Готово" "Done")"
    if [[ "$greetd" == yes ]]; then
        echo "$(t "Перезагрузитесь: sudo reboot" "Reboot: sudo reboot")"
        echo "$(t "После загрузки появится экран входа Пёрышка; введите пароль. Выход из сеанса" "After the boot the Feather login screen appears; enter your password. Logging out")"
        echo "$(t "(меню Пёрышка → «Выйти из Пёрышка…») возвращает на этот экран." "(Feather menu → “Log Out…”) brings you back to this screen.")"
        (( tuigreet )) || echo "$(t "Экран входа открывается для $login_user; для нескольких пользователей: --tuigreet." "The login screen opens for $login_user; for several users use --tuigreet.")"
        (( has_dm )) && warn "$(t "greetd включён вместо прежнего экрана входа ($dm); вернуть: sudo systemctl enable --force $dm" "greetd is enabled instead of the previous login screen ($dm); to go back: sudo systemctl enable --force $dm")"
    elif (( has_dm )); then
        echo "$(t "Ваш рабочий стол остаётся как был: Пёрышко — ещё один сеанс на экране входа." "Your desktop stays as it was: Feather is one more session on the login screen.")"
        case "$dm" in
            sddm) echo "$(t "Экран входа SDDM: выйдите из сеанса, внизу слева выберите сеанс" "SDDM login screen: log out and pick the session at the bottom left:")" ;;
            gdm) echo "$(t "Экран входа GDM: выйдите из сеанса, выберите пользователя, нажмите шестерёнку внизу справа и выберите" "GDM login screen: log out, pick your user, click the gear at the bottom right and choose")" ;;
            lightdm) echo "$(t "Экран входа LightDM: выйдите из сеанса и в списке сеансов выберите" "LightDM login screen: log out and in the session list choose")" ;;
            ly|ly@*) echo "$(t "Экран входа ly: выйдите из сеанса, стрелками влево/вправо в строке сеанса выберите" "ly login screen: log out and use the left/right arrows on the session line to choose")" ;;
            greetd) echo "$(t "Экран входа greetd: выберите сеанс (в tuigreet — клавиша F3)" "greetd login screen: choose the session (in tuigreet, key F3)")" ;;
            *) echo "$(t "Экран входа $dm: выйдите из сеанса и в списке сеансов выберите" "$dm login screen: log out and in the session list choose")" ;;
        esac
        printf '    \033[1m%s\033[0m %s\n' "$(t '«Пёрышко (прямой вывод DRM/KMS)»' '“Feather (direct DRM/KMS output)”')" "$(t 'и войдите как обычно.' 'and log in as usual.')"
    else
        echo "$(t "Экран входа не ставился. На консоли (Ctrl+Alt+F3) войдите и наберите:  feather-session" "No login screen was installed. On a console (Ctrl+Alt+F3) log in and type:  feather-session")"
    fi
    echo
    echo "$(t "  Попробовать в окне, не выходя из сеанса:  feather-de --windowed" "  Try it in a window without logging out:  feather-de --windowed")"
    echo "$(t "  Аварийный выход из Пёрышка:  Ctrl+Alt+Backspace" "  Emergency exit from Feather: Ctrl+Alt+Backspace")"
    echo "$(t "  Лог сеанса:                  ~/.local/state/feather-de/drm.log" "  Session log:                 ~/.local/state/feather-de/drm.log")"
    echo "$(t "  Обновления:                  sudo pacman -Syu" "  Updates:                     sudo pacman -Syu")"
    echo "$(t "  Удаление:                    sudo pacman -Rns feather-de" "  Uninstall:                   sudo pacman -Rns feather-de")"
}

main "$@"
