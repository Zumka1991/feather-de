#!/usr/bin/env bash
# Установка сеансов Wayland, правила PAM и (по желанию) greetd-конфигурации Пёрышка.
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"

echo "🪶 Установка файлов сеанса Пёрышка (Feather DE)..."
sudo install -Dm644 "$PROJECT_DIR/session/feather-de.desktop" /usr/share/wayland-sessions/feather-de.desktop
sudo install -Dm644 "$PROJECT_DIR/session/feather-de-drm.desktop" /usr/share/wayland-sessions/feather-de-drm.desktop
sudo install -Dm644 "$PROJECT_DIR/session/feather-lock.pam" /etc/pam.d/feather-lock

if [[ "${1:-}" == "--greetd" ]]; then
    sudo install -Dm644 "$PROJECT_DIR/session/feather-greeter.toml" /etc/greetd/config.toml
    echo "✅ Конфигурация greetd установлена в /etc/greetd/config.toml"
fi

echo "✅ Готово! Доступны флаги запуска: --boot-splash и --login (--greeter)."
