#!/usr/bin/env bash
# Сборка автономного ИИ-модуля (PyInstaller) для Linux.
# Аналог build.ps1 для Windows. Запускать из корня проекта:
#   bash sidecar/build.sh
# Результат: sidecar/dist/fm-sidecar (+ библиотеки рядом).
set -euo pipefail

cd "$(dirname "$0")/.."

echo "== Проверяю Python =="
PY=${PYTHON:-python3}
"$PY" --version

echo "== Ставлю зависимости и PyInstaller =="
# В Ubuntu 24.04+ системный Python защищён (PEP 668). Работаем в venv —
# так ничего не ломаем в системе и получаем предсказуемую сборку.
VENV=sidecar/.venv
if [ ! -d "$VENV" ]; then
  "$PY" -m venv "$VENV" || {
    echo "Не удалось создать venv. Установи: sudo apt install python3-venv" >&2
    exit 1
  }
fi
# shellcheck disable=SC1091
source "$VENV/bin/activate"
python -m pip install --upgrade pip
python -m pip install -r sidecar/requirements.txt
python -m pip install pyinstaller

echo "== Сборка (долго: onnxruntime большой) =="
python -m PyInstaller sidecar/fm-sidecar.spec --noconfirm \
  --distpath sidecar/dist_build --workpath sidecar/build

BUILT=sidecar/dist_build/fm-sidecar
if [ ! -x "$BUILT/fm-sidecar" ]; then
  echo "Сборка не удалась: $BUILT/fm-sidecar не найден" >&2
  exit 1
fi

# PLACEHOLDER.txt — единственный отслеживаемый файл в sidecar/dist: без него
# папки нет в свежем клоне и сборка Tauri падает на ресурсах.
KEEP=sidecar/dist/PLACEHOLDER.txt
KEEP_TEXT=""
[ -f "$KEEP" ] && KEEP_TEXT=$(cat "$KEEP")

rm -rf sidecar/dist
mkdir -p sidecar/dist
cp -r "$BUILT/." sidecar/dist/
rm -rf sidecar/dist_build

if [ -n "$KEEP_TEXT" ]; then
  printf '%s' "$KEEP_TEXT" > "$KEEP"
else
  echo "Built sidecar goes here. See sidecar/BUILD.md." > "$KEEP"
fi

SIZE=$(du -sh sidecar/dist | cut -f1)
echo
echo "== Готово =="
echo "Бинарник: sidecar/dist/fm-sidecar  (пакет ~$SIZE)"
echo "Перезапусти приложение: Настройки → Хранение → «ИИ-модуль» должен сказать, что Python не требуется."
