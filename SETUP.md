# Установка окружения и запуск (с нуля)

> Ниже — инструкция для **Windows**. Для **Ubuntu/Linux** смотри отдельный
> раздел в конце файла: [Ubuntu / Linux](#ubuntu--linux).

## Windows

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

# Ubuntu / Linux

Проверялось на Ubuntu 22.04 и 24.04 (GNOME). Node.js так же не нужен.

## Шаг 1. Системные библиотеки

```bash
sudo apt update
sudo apt install -y build-essential curl wget file libssl-dev \
  libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libxdo-dev \
  libayatana-appindicator3-dev pkg-config
```

На Ubuntu 22.04 пакет вебвью может называться `libwebkit2gtk-4.0-dev` — ставь
тот, который найдётся.

## Шаг 2. Видео: кодеки и ffmpeg (важно)

Вебвью в Linux — WebKitGTK, и «из коробки» он не проигрывает MP4/H.264. Без
кодеков видео не будет воспроизводиться в просмотрщике:

```bash
sudo apt install -y gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly gstreamer1.0-libav
```

Дополнительно поставь **ffmpeg** — это заметный плюс Linux-версии:

```bash
sudo apt install -y ffmpeg
```

С ним кадры для превью и ИИ-индексации видео извлекает сам бэкенд, а не
браузерный движок: индексация точнее и не зависит от кодеков вебвью (в Windows
ffmpeg обычно нет, и кадры снимает интерфейс — это медленнее).

## Шаг 3. Rust и Tauri CLI

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
cargo install tauri-cli --locked
```

## Шаг 4. Python для ИИ (если не используешь готовый бинарник ИИ-модуля)

```bash
sudo apt install -y python3 python3-pip python3-venv
```

В Ubuntu 24.04+ системный Python защищён (PEP 668). Приложение это понимает:
кнопка «⬇ Установить ИИ-компоненты» при отказе повторит установку с
`--break-system-packages`. Чище — собрать автономный ИИ-модуль (шаг 6).

## Шаг 5. Запуск

```bash
cd ~/FurryManager
cargo tauri dev
```

## Шаг 6. Сборка дистрибутива (.deb / AppImage)

```bash
bash sidecar/build.sh     # автономный ИИ-модуль (долго, опционально)
bash release.sh           # приложение + пакеты
```

Готовое — в `src-tauri/target/release/bundle/` (deb, appimage).
Установка: `sudo apt install ./FurryManager_0.8.0_amd64.deb`

## Отличия Linux-версии

| Функция | Состояние |
|---|---|
| Галерея, теги, коллекции, группы, поиск, ИИ | Работает так же |
| Сервер для телефона, PWA, Telegram-бот | Работает так же |
| Шторка при сворачивании | Работает |
| Отдельное событие «Win+L» (блокировка экрана) | Только Windows; шторка всё равно срабатывает при сворачивании |
| Скрытие папок библиотеки от файлового менеджера | Через `.hidden` и `.nomedia` (GNOME Files, галереи), а не атрибутами файлов |
| Видео (превью, индексация, GIF) | Нужны кодеки из шага 2; с установленным ffmpeg кадры извлекает бэкенд — работает лучше, чем в Windows |
| Вставка картинки из буфера | Зависит от окружения: под Wayland WebKitGTK иногда не отдаёт картинку из буфера — тогда используй импорт файлом |
| Tailscale | Нужно один раз разрешить: `sudo tailscale set --operator=$USER` |

# Если что-то пошло не так

Скопируй текст ошибки из PowerShell (или терминала) целиком и пришли мне — починю. Типичные случаи:

| Симптом | Причина |
|---|---|
| `link.exe not found` | Не установлены Build Tools (шаг 1) или не перезапущен PowerShell |
| `dlltool.exe: program not found` | Rust стоит с GNU-тулчейном. Переключить на MSVC: `rustup toolchain install stable-x86_64-pc-windows-msvc` затем `rustup default stable-x86_64-pc-windows-msvc` |
| `cargo: command not found` | Не перезапущен PowerShell после шага 2 |
| Белое окно без интерфейса | Нет WebView2 (шаг 4) |
| Картинки не отображаются | Пришли мне — вероятно, надо поправить scope asset-протокола |
| **Linux:** `error: failed to run custom build command for webkit2gtk` | Не установлен `libwebkit2gtk-4.1-dev` (шаг 1) |
| **Linux:** видео не проигрывается, нет превью | Не установлены gstreamer-кодеки (шаг 2) |
| **Linux:** `externally-managed-environment` при установке ИИ | PEP 668 — приложение повторит с `--break-system-packages`; либо собери автономный ИИ-модуль |
