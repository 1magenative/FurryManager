# Установка окружения и запуск (Windows, с нуля)

Node.js **не нужен** — фронтенд без сборщика. Ставим только Rust и его инструменты. Всё делается один раз, займёт ~30–60 минут (в основном ожидание установок).

## Шаг 1. Инструменты сборки C++ (нужны Rust'у)

Открой **PowerShell** (Пуск → набери «powershell» → Enter) и выполни:

```powershell
winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

Ждём (10–20 минут, ~6 ГБ). Если `winget` не найден — установи «App Installer» из Microsoft Store и повтори.

*Альтернатива вручную:* скачай [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/), при установке отметь галочку **«Разработка классических приложений на C++»**.

## Шаг 2. Rust

```powershell
winget install Rustlang.Rustup
```

Затем **закрой и заново открой PowerShell** (чтобы обновился PATH) и проверь:

```powershell
rustc --version
```

Должно показать версию (например `rustc 1.8x.x`). Если rustup спросит про установку — выбирай вариант по умолчанию (1, MSVC).

## Шаг 3. Tauri CLI

```powershell
cargo install tauri-cli --version "^2" --locked
```

Это компиляция из исходников — 10–20 минут, один раз.

## Шаг 4. WebView2

На Windows 10/11 обычно уже установлен (его использует Edge). Если приложение не запустится с ошибкой про WebView2 — установи [Evergreen Runtime](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) (раздел Evergreen Bootstrapper).

## Шаг 5 (необязательно). ffmpeg — превью для видео

Без него видео импортируются, но без миниатюр (будет значок 🎬).

```powershell
winget install Gyan.FFmpeg
```

---

# Запуск

```powershell
cd D:\FurryManager
cargo tauri dev
```

Первая сборка — 5–15 минут (компилируются все зависимости), дальше — секунды. Откроется окно приложения с онбордингом.

**Важно:** папку для библиотеки в онбординге выбирай НЕ `D:\FurryManager` (там код), а отдельную — например `D:\ArtLibrary`.

# Сборка установщика (когда захочешь)

```powershell
cd D:\FurryManager
cargo tauri build
```

Готовый установщик появится в `src-tauri\target\release\bundle\nsis\`.

# v0.2 — ИИ-модуль (умный поиск и автотеги)

Нужен Python 3.11+ (проверь: `python --version`). Дальше проще всего **из самого приложения**: кнопка 🧠 → «⬇ Установить ИИ-компоненты» — поставит пакеты и скачает модели с прогрессом в углу окна.

Вручную (эквивалентно):

```powershell
cd D:\FurryManager
python -m pip install -r sidecar\requirements.txt
```

Это ~300 МБ пакетов. Дальше в приложении: кнопка **🧠** справа вверху → «Проиндексировать библиотеку». Первая индексация дополнительно скачает модели (~1 ГБ, один раз, кэш HuggingFace). После неё:

- **Умный поиск** — кнопка 🧠 в строке поиска, описывай сцены словами (лучше по-английски: «red fox in jacket»)
- **Автотеги** — раса, одежда, рейтинг и т.п. появятся у артов автоматически
- **«Похожие в библиотеке»** начнёт искать по стилю/содержанию, а не только дубликаты

Новые импортированные файлы индексируются автоматически в фоне.

# v0.3 — расширение браузера (качалка)

1. Обнови зависимости ИИ (добавился gallery-dl): 🧠 → «Установить ИИ-компоненты», либо `python -m pip install -r sidecar\requirements.txt`
2. Chrome/Edge → Расширения → включи «Режим разработчика» → «Загрузить распакованное» → выбери папку `D:\FurryManager\extension`
3. В FurryManager: ⚙ Настройки → скопируй токен
4. ПКМ по иконке расширения → Параметры → вставь токен → Сохранить
5. Готово: на любом сайте ПКМ → «🦊 Сохранить в FurryManager». Приложение должно быть запущено.

Для FA/e621 с возрастными фильтрами gallery-dl может брать куки из браузера — если что-то не скачивается, напиши, настроим.

# Если что-то пошло не так

Скопируй текст ошибки из PowerShell целиком и пришли мне — починю. Типичные случаи:

| Симптом | Причина |
|---|---|
| `link.exe not found` | Не установлены Build Tools (шаг 1) или не перезапущен PowerShell |
| `dlltool.exe: program not found` | Rust стоит с GNU-тулчейном. Переключить на MSVC: `rustup toolchain install stable-x86_64-pc-windows-msvc` затем `rustup default stable-x86_64-pc-windows-msvc` |
| `cargo: command not found` | Не перезапущен PowerShell после шага 2 |
| Белое окно без интерфейса | Нет WebView2 (шаг 4) |
| Картинки не отображаются | Пришли мне — вероятно, надо поправить scope asset-протокола |
