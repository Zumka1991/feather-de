#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="/home/zumka/Documents/antigravity/silly-tesla"
cd "$PROJECT_DIR"

echo "🔨 Сборка новой версии Пёрышко DE (release)..."
cargo build --release

echo "🔄 Горячая замена запущенного бинарника без выхода из сессии..."
touch /tmp/feather-restart
pkill -x feather-de || true
echo "✅ Готово!"
