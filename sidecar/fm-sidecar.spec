# PyInstaller-спека для ML-сайдкара FurryManager.
# Собирает автономный fm-sidecar(.exe): Python и pip-пакеты пользователю не нужны.
#
# Сборка:  pyinstaller sidecar/fm-sidecar.spec --noconfirm --distpath sidecar/dist
#
# Модели (CLIP ~600 МБ, WD14 ~450 МБ) в бинарник НЕ вшиваются — они по-прежнему
# качаются при первом использовании в кэш HuggingFace. Это осознанно: иначе
# установщик распух бы на гигабайт.

from PyInstaller.utils.hooks import collect_all, collect_submodules

datas, binaries, hiddenimports = [], [], []

# Пакеты с нетривиальными данными/нативными библиотеками — собираем целиком
for pkg in ("fastembed", "onnxruntime", "tokenizers", "huggingface_hub", "PIL", "pillow_heif"):
    try:
        d, b, h = collect_all(pkg)
        datas += d
        binaries += b
        hiddenimports += h
    except Exception:
        pass  # пакет не установлен — соответствующая функция просто отключится

# gallery-dl тянет экстракторы динамически: без явного сбора подмодулей
# скачивание работать не будет
try:
    hiddenimports += collect_submodules("gallery_dl")
    d, b, h = collect_all("gallery_dl")
    datas += d
    binaries += b
    hiddenimports += h
except Exception:
    pass

hiddenimports += ["numpy", "requests", "certifi"]

a = Analysis(
    ["main.py"],
    pathex=[],
    binaries=binaries,
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    runtime_hooks=[],
    excludes=["tkinter", "matplotlib", "PyQt5", "PySide6", "IPython", "pytest"],
    noarchive=False,
)
pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name="fm-sidecar",
    console=True,          # общение идёт по stdin/stdout — консоль обязательна
    disable_windowed_traceback=False,
    strip=False,
    upx=False,             # UPX ломает onnxruntime
)
coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=False,
    name="fm-sidecar",
)
