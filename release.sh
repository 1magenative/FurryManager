#!/usr/bin/env bash
# Сборка дистрибутива FurryManager для Linux (.deb и AppImage).
# Аналог release.ps1 для Windows. Из корня проекта:
#   bash release.sh              — собрать всё (при необходимости соберёт ИИ-модуль)
#   bash release.sh --skip-sidecar
set -euo pipefail
cd "$(dirname "$0")"

SKIP_SIDECAR=0
[ "${1:-}" = "--skip-sidecar" ] && SKIP_SIDECAR=1

SIDECAR_BIN=sidecar/dist/fm-sidecar

if [ "$SKIP_SIDECAR" -eq 0 ]; then
  if [ -x "$SIDECAR_BIN" ]; then
    echo "== ИИ-модуль уже собран, использую его =="
    echo "   (удали sidecar/dist, чтобы пересобрать)"
  else
    echo "== Собираю автономный ИИ-модуль (долго) =="
    bash sidecar/build.sh
  fi
fi

if [ -x "$SIDECAR_BIN" ]; then
  echo "== ИИ-модуль будет вложен: пользователю не нужен Python =="
else
  echo "== ИИ-модуля нет: для ИИ-функций пользователю понадобится Python =="
  mkdir -p sidecar/dist
  [ -f sidecar/dist/PLACEHOLDER.txt ] || \
    echo "Built sidecar goes here. See sidecar/BUILD.md." > sidecar/dist/PLACEHOLDER.txt
fi

echo "== Проверяю системные зависимости =="
MISSING=""
for pkg in libwebkit2gtk-4.1-dev build-essential libssl-dev librsvg2-dev; do
  dpkg -s "$pkg" >/dev/null 2>&1 || MISSING="$MISSING $pkg"
done
if [ -n "$MISSING" ]; then
  echo "Не хватает пакетов для сборки:$MISSING" >&2
  echo "Установи их:  sudo apt install$MISSING" >&2
  exit 1
fi

echo "== Сборка приложения и пакетов =="
cargo tauri build

echo
echo "== Готово =="
find src-tauri/target/release/bundle -maxdepth 2 -type f \
  \( -name '*.deb' -o -name '*.AppImage' -o -name '*.rpm' \) -printf '%p  (%kK)\n' || true
echo
echo "Установка .deb:      sudo apt install ./<файл>.deb"
echo "Запуск AppImage:     chmod +x <файл>.AppImage && ./<файл>.AppImage"
