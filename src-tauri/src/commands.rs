use crate::{db, import, similar};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};

pub struct AppState {
    pub conn: Mutex<Option<Connection>>,
    pub root: Mutex<Option<PathBuf>>,
}

// ---------- Конфиг приложения (папка библиотеки, режим UI) ----------

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Config {
    pub library_root: Option<String>,
    pub mode: Option<String>, // basic | advanced
    #[serde(default)]
    pub python_path: Option<String>, // путь к python для ИИ-сайдкара
    #[serde(default)]
    pub nsfw_split: Option<bool>, // вкладки SFW/NSFW
    #[serde(default)]
    pub http_port: Option<u16>, // порт для браузерного расширения
    #[serde(default)]
    pub http_token: Option<String>, // токен для браузерного расширения
    #[serde(default)]
    pub default_nsfw: Option<i64>, // фильтр при запуске: None=все, 0=SFW, 1=NSFW
    #[serde(default)]
    pub watched: Vec<WatchedFolder>, // отслеживаемые папки (watcher)
    #[serde(default)]
    pub lossless: Option<bool>, // конвертировать PNG → WebP (lossless) при копировании
    #[serde(default)]
    pub lan: Option<bool>, // доступ с телефона: сервер слушает локальную сеть
    #[serde(default)]
    pub pin_hash: Option<String>, // sha256(salt + pin); None = ПИН выключен
    #[serde(default)]
    pub pin_salt: Option<String>,
    #[serde(default)]
    pub lock_mode: Option<String>, // off | hidden (сворачивание/Win+L) | blur (любой расфокус)
    #[serde(default)]
    pub recovery_hash: Option<String>, // хеш кода восстановления ПИН
    #[serde(default)]
    pub pin_reset_at: Option<i64>, // unix: когда запрошен отложенный сброс ПИН
    #[serde(default)]
    pub browser_cookies: Option<String>, // chrome|firefox|edge|"" — куки для gallery-dl
}

/// Папка под наблюдением: новые файлы автоматически импортируются.
#[derive(Serialize, Deserialize, Clone)]
pub struct WatchedFolder {
    pub path: String,
    #[serde(default)]
    pub in_place: bool, // true — не копировать, оставить оригиналы на месте
}

/// Гарантирует, что в конфиге есть токен и порт для расширения.
pub fn ensure_http_credentials() -> (u16, String) {
    let mut cfg = load_config();
    let port = cfg.http_port.unwrap_or(7877);
    let token = match &cfg.http_token {
        Some(t) if !t.is_empty() => t.clone(),
        _ => {
            // Локальный токен; криптостойкость не критична — сервер слушает только 127.0.0.1
            let seed = format!(
                "{:x}{:x}{:x}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
                &cfg as *const _ as usize
            );
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(seed.as_bytes());
            let t = hex::encode(&hasher.finalize()[..16]);
            cfg.http_port = Some(port);
            cfg.http_token = Some(t.clone());
            let _ = save_config(&cfg);
            t
        }
    };
    (port, token)
}

/// Публичная обёртка для server.rs.
pub fn sidecar_cfg_pub() -> Result<(PathBuf, String), String> {
    sidecar_cfg()
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("FurryManager")
        .join("config.json")
}

pub fn load_config() -> Config {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(c: &Config) -> Result<(), String> {
    let p = config_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(c).unwrap()).map_err(|e| e.to_string())
}

// ---------- Общие структуры ----------

#[derive(Serialize)]
pub struct FileCard {
    pub id: i64,
    pub kind: String,
    pub ext: String,
    pub width: i64,
    pub height: i64,
    pub size: i64,
    pub path: String,
    pub thumb: Option<String>,
    pub group_id: Option<i64>,
    pub group_count: i64, // размер группы (0/1 = не группа)
    pub group_name: Option<String>,
    pub stack2: Option<String>, // превью 2-й страницы (для стопки в галерее)
    pub stack3: Option<String>, // превью 3-й страницы
}

// Групповые колонки — строго в конец (индексы 8–12); дополнительные колонки
// конкретных запросов (sha256, embedding) идут после них (индекс 13).
const CARD_COLS: &str = "f.id, f.kind, f.ext, f.width, f.height, f.size, f.rel_path, f.thumb_rel, \
    f.group_id, COALESCE((SELECT COUNT(*) FROM files fg WHERE fg.group_id = f.group_id), 0), \
    (SELECT g.name FROM groups g WHERE g.id = f.group_id), \
    (SELECT COALESCE(f2.thumb_rel, f2.rel_path) FROM files f2 WHERE f2.group_id = f.group_id \
     ORDER BY COALESCE(f2.group_pos, 0), f2.id LIMIT 1 OFFSET 1), \
    (SELECT COALESCE(f3.thumb_rel, f3.rel_path) FROM files f3 WHERE f3.group_id = f.group_id \
     ORDER BY COALESCE(f3.group_pos, 0), f3.id LIMIT 1 OFFSET 2)";

/// Обложка группы: сам файл вне группы, либо первый по порядку внутри неё.
pub(crate) const GROUP_COVER_WHERE: &str = " AND (f.group_id IS NULL OR NOT EXISTS (\
    SELECT 1 FROM files f2 WHERE f2.group_id = f.group_id AND \
    (COALESCE(f2.group_pos, 0) < COALESCE(f.group_pos, 0) OR \
     (COALESCE(f2.group_pos, 0) = COALESCE(f.group_pos, 0) AND f2.id < f.id))))";

fn card_from_row(root: &Path, row: &rusqlite::Row) -> rusqlite::Result<FileCard> {
    let rel: String = row.get(6)?;
    let thumb: Option<String> = row.get(7)?;
    Ok(FileCard {
        id: row.get(0)?,
        kind: row.get(1)?,
        ext: row.get(2)?,
        width: row.get(3)?,
        height: row.get(4)?,
        size: row.get(5)?,
        path: root.join(rel).to_string_lossy().into_owned(),
        thumb: thumb.map(|t| root.join(t).to_string_lossy().into_owned()),
        group_id: row.get(8)?,
        group_count: row.get(9)?,
        group_name: row.get(10)?,
        stack2: row
            .get::<_, Option<String>>(11)?
            .map(|t| root.join(t).to_string_lossy().into_owned()),
        stack3: row
            .get::<_, Option<String>>(12)?
            .map(|t| root.join(t).to_string_lossy().into_owned()),
    })
}

fn get_root(state: &State<AppState>) -> Result<PathBuf, String> {
    state
        .root
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "Библиотека ещё не настроена".to_string())
}

// ---------- Настройка / онбординг ----------

#[derive(Serialize)]
pub struct SetupState {
    pub library_root: Option<String>,
    pub mode: String,
    pub nsfw_split: bool,
    pub http_port: u16,
    pub http_token: String,
    pub default_nsfw: Option<i64>,
    pub lossless: bool,
    pub lan: bool,
    pub pin_enabled: bool,
    pub lock_mode: String,
    pub pin_reset_remaining: Option<i64>, // сек до отложенного сброса ПИН (если запрошен)
    pub browser_cookies: String,
    pub app_version: String,
    pub library_ok: bool, // папка библиотеки на месте и база открывается
}

const PIN_RESET_DELAY: i64 = 24 * 3600; // отложенный сброс ПИН — сутки

#[tauri::command]
pub fn get_setup_state() -> SetupState {
    let mut c = load_config();
    // Отложенный сброс ПИН созрел? Отключаем ПИН.
    let mut pin_reset_remaining = None;
    if let Some(at) = c.pin_reset_at {
        let left = at + PIN_RESET_DELAY - unix_now();
        if left <= 0 {
            c.pin_hash = None;
            c.pin_salt = None;
            c.recovery_hash = None;
            c.pin_reset_at = None;
            let _ = save_config(&c);
        } else {
            pin_reset_remaining = Some(left);
        }
    }
    // Библиотека могла переехать/остаться на отключённом диске — проверяем
    let library_ok = c
        .library_root
        .as_ref()
        .map(|r| {
            let p = std::path::Path::new(r);
            p.is_dir() && db::open(&p.join("library.db")).is_ok()
        })
        .unwrap_or(false);

    SetupState {
        library_root: c.library_root,
        mode: c.mode.unwrap_or_else(|| "basic".into()),
        nsfw_split: c.nsfw_split.unwrap_or(false),
        http_port: c.http_port.unwrap_or(7877),
        http_token: c.http_token.unwrap_or_default(),
        default_nsfw: c.default_nsfw,
        lossless: c.lossless.unwrap_or(false),
        lan: c.lan.unwrap_or(false),
        pin_enabled: c.pin_hash.is_some(),
        lock_mode: c.lock_mode.unwrap_or_else(|| "off".into()),
        pin_reset_remaining,
        browser_cookies: c.browser_cookies.unwrap_or_default(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        library_ok,
    }
}

// ---------- Экспорт / импорт библиотеки (zip) ----------

/// Переносимая часть настроек (без машинно-специфичных путей и токенов).
#[derive(Serialize, Deserialize, Default)]
struct PortableConfig {
    mode: Option<String>,
    nsfw_split: Option<bool>,
    default_nsfw: Option<i64>,
    lossless: Option<bool>,
}

/// Экспорт всей библиотеки в zip: база, файлы, превью и переносимые настройки.
/// Без сжатия (медиа уже сжаты) — быстро. Прогресс — событие "export-progress".
#[tauri::command]
pub async fn export_library(
    app: AppHandle,
    state: State<'_, AppState>,
    dest: String,
) -> Result<i64, String> {
    let root = get_root(&state)?;
    // Скидываем WAL в основную базу, чтобы library.db был самодостаточным
    {
        let guard = state.conn.lock().unwrap();
        let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
    let cfg = load_config();
    let portable = PortableConfig {
        mode: cfg.mode,
        nsfw_split: cfg.nsfw_split,
        default_nsfw: cfg.default_nsfw,
        lossless: cfg.lossless,
    };

    tauri::async_runtime::spawn_blocking(move || -> Result<i64, String> {
        use std::io::Write;
        use zip::write::FileOptions;
        let f = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
        let mut z = zip::ZipWriter::new(f);
        let opts: FileOptions =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);

        // Список файлов: база + store + thumbs
        let mut entries: Vec<(PathBuf, String)> = Vec::new();
        entries.push((root.join("library.db"), "library.db".into()));
        for dir in ["store", "thumbs"] {
            let base = root.join(dir);
            if base.exists() {
                for e in walkdir::WalkDir::new(&base).into_iter().filter_map(|e| e.ok()) {
                    if e.file_type().is_file() {
                        let rel = e
                            .path()
                            .strip_prefix(&root)
                            .map_err(|e| e.to_string())?
                            .to_string_lossy()
                            .replace('\\', "/");
                        entries.push((e.path().to_path_buf(), rel));
                    }
                }
            }
        }
        let total = entries.len() as i64;
        let mut done = 0i64;
        for (abs, name) in entries {
            z.start_file(&name, opts).map_err(|e| e.to_string())?;
            let mut src = std::fs::File::open(&abs).map_err(|e| e.to_string())?;
            std::io::copy(&mut src, &mut z).map_err(|e| e.to_string())?;
            done += 1;
            if done % 50 == 0 || done == total {
                let _ = app.emit(
                    "export-progress",
                    serde_json::json!({ "done": done, "total": total }),
                );
            }
        }
        // Переносимые настройки
        z.start_file("furrymanager.json", opts).map_err(|e| e.to_string())?;
        let j = serde_json::to_vec_pretty(&portable).map_err(|e| e.to_string())?;
        z.write_all(&j).map_err(|e| e.to_string())?;
        z.finish().map_err(|e| e.to_string())?;
        Ok(total)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Импорт библиотеки из zip-архива в ТЕКУЩУЮ папку библиотеки.
/// База заменяется базой из архива; файлы докопируются. Прогресс — "export-progress".
#[tauri::command]
pub async fn import_library(
    app: AppHandle,
    state: State<'_, AppState>,
    src: String,
) -> Result<i64, String> {
    let root = get_root(&state)?;
    // Закрываем текущую базу — её файл будет заменён
    {
        let mut guard = state.conn.lock().unwrap();
        *guard = None;
    }

    let root2 = root.clone();
    let res = tauri::async_runtime::spawn_blocking(move || -> Result<i64, String> {
        let f = std::fs::File::open(&src).map_err(|e| e.to_string())?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| format!("Не открылся архив: {e}"))?;
        // Старую БД и её WAL-хвосты убираем
        let _ = std::fs::remove_file(root2.join("library.db"));
        let _ = std::fs::remove_file(root2.join("library.db-wal"));
        let _ = std::fs::remove_file(root2.join("library.db-shm"));

        let total = z.len() as i64;
        let mut done = 0i64;
        let mut portable: Option<PortableConfig> = None;
        for i in 0..z.len() {
            let mut entry = z.by_index(i).map_err(|e| e.to_string())?;
            let name = entry.name().to_string();
            if name == "furrymanager.json" {
                let mut s = String::new();
                use std::io::Read;
                let _ = entry.read_to_string(&mut s);
                portable = serde_json::from_str(&s).ok();
                continue;
            }
            // Защита от zip-slip: только простые относительные пути
            if name.contains("..") {
                continue;
            }
            let dest = root2.join(name.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(dir) = dest.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            // Файлы контент-адресуемые: существующие можно не перезаписывать (кроме БД)
            if dest.exists() && !name.ends_with("library.db") {
                done += 1;
                continue;
            }
            let mut out = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
            done += 1;
            if done % 50 == 0 || done == total {
                let _ = app.emit(
                    "export-progress",
                    serde_json::json!({ "done": done, "total": total }),
                );
            }
        }
        // Переносимые настройки — поверх текущего конфига
        if let Some(p) = portable {
            let mut c = load_config();
            if p.mode.is_some() { c.mode = p.mode; }
            if p.nsfw_split.is_some() { c.nsfw_split = p.nsfw_split; }
            c.default_nsfw = p.default_nsfw;
            if p.lossless.is_some() { c.lossless = p.lossless; }
            let _ = save_config(&c);
        }
        Ok(done)
    })
    .await
    .map_err(|e| e.to_string())?;

    // Открываем базу заново (независимо от исхода — иначе приложение без БД)
    let conn = crate::db::open(&root.join("library.db")).map_err(|e| e.to_string())?;
    *state.conn.lock().unwrap() = Some(conn);
    res
}

// ---------- ПИН-код и приватность ----------

fn pin_digest(salt: &str, pin: &str) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(salt.as_bytes());
    h.update(pin.as_bytes());
    hex::encode(h.finalize())
}

fn fresh_salt() -> String {
    format!(
        "{:x}{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

/// Установка/смена/выключение ПИН. current обязателен, если ПИН уже задан.
/// new_pin = None — выключить. При установке возвращает одноразовый код
/// восстановления: {"enabled": bool, "recovery": Option<String>}.
#[tauri::command]
pub fn pin_set(current: Option<String>, new_pin: Option<String>) -> Result<serde_json::Value, String> {
    let mut c = load_config();
    if let (Some(hash), Some(salt)) = (&c.pin_hash, &c.pin_salt) {
        let cur = current.unwrap_or_default();
        if pin_digest(salt, &cur) != *hash {
            return Err("Текущий ПИН неверный".into());
        }
    }
    match new_pin {
        None => {
            c.pin_hash = None;
            c.pin_salt = None;
            c.recovery_hash = None;
            c.pin_reset_at = None;
            save_config(&c)?;
            Ok(serde_json::json!({ "enabled": false, "recovery": null }))
        }
        Some(p) => {
            let p = p.trim().to_string();
            if p.len() < 4 {
                return Err("ПИН должен быть не короче 4 символов".into());
            }
            let salt = fresh_salt();
            c.pin_hash = Some(pin_digest(&salt, &p));
            c.pin_salt = Some(salt.clone());
            // Код восстановления: показывается один раз, храним только хеш
            let recovery: String = {
                let seed = pin_digest(&fresh_salt(), &format!("{p}{salt}"));
                seed.chars().take(10).collect::<String>().to_uppercase()
            };
            c.recovery_hash = Some(pin_digest(&salt, &recovery));
            c.pin_reset_at = None;
            save_config(&c)?;
            Ok(serde_json::json!({ "enabled": true, "recovery": recovery }))
        }
    }
}

/// Проверка ПИН. Успешный ввод отменяет отложенный сброс (если был запрошен).
#[tauri::command]
pub fn pin_verify(pin: String) -> bool {
    let mut c = load_config();
    let ok = match (&c.pin_hash, &c.pin_salt) {
        (Some(hash), Some(salt)) => pin_digest(salt, &pin) == *hash,
        _ => true, // ПИН не задан — пропускаем
    };
    if ok && c.pin_reset_at.is_some() {
        c.pin_reset_at = None; // хозяин на месте — сброс отменяем
        let _ = save_config(&c);
    }
    ok
}

/// Восстановление по коду: правильный код отключает ПИН.
#[tauri::command]
pub fn pin_recover(code: String) -> Result<bool, String> {
    let mut c = load_config();
    let (Some(rh), Some(salt)) = (&c.recovery_hash, &c.pin_salt) else {
        return Err("Код восстановления не был создан".into());
    };
    if pin_digest(salt, code.trim().to_uppercase().as_str()) != *rh {
        return Ok(false);
    }
    c.pin_hash = None;
    c.pin_salt = None;
    c.recovery_hash = None;
    c.pin_reset_at = None;
    save_config(&c)?;
    Ok(true)
}

/// Запросить отложенный сброс ПИН (сработает через сутки, если ПИН
/// ни разу не будет введён). Возвращает срок в секундах.
#[tauri::command]
pub fn pin_request_reset() -> Result<i64, String> {
    let mut c = load_config();
    if c.pin_hash.is_none() {
        return Err("ПИН не установлен".into());
    }
    if c.pin_reset_at.is_none() {
        c.pin_reset_at = Some(unix_now());
        save_config(&c)?;
    }
    Ok(c.pin_reset_at.unwrap() + PIN_RESET_DELAY - unix_now())
}

#[tauri::command]
pub fn set_settings(
    mode: String,
    nsfw_split: bool,
    default_nsfw: Option<i64>,
    lossless: bool,
    lan: bool,
    lock_mode: String,
    browser_cookies: String,
) -> Result<(), String> {
    let mut c = load_config();
    c.browser_cookies = Some(match browser_cookies.as_str() {
        "chrome" | "firefox" | "edge" | "brave" | "opera" | "vivaldi" | "chromium" => {
            browser_cookies
        }
        _ => String::new(),
    });
    c.mode = Some(mode);
    c.nsfw_split = Some(nsfw_split);
    c.default_nsfw = default_nsfw;
    c.lossless = Some(lossless);
    c.lan = Some(lan);
    c.lock_mode = Some(match lock_mode.as_str() {
        "hidden" => "hidden".into(),
        "blur" => "blur".into(),
        _ => "off".into(),
    });
    save_config(&c)
}

/// QR для настройки телефона: адрес сервера в локальной сети + токен.
#[tauri::command]
pub fn lan_qr() -> Result<serde_json::Value, String> {
    let cfg = load_config();
    let port = cfg.http_port.unwrap_or(7877);
    let token = cfg.http_token.unwrap_or_default();
    let ip = local_ip().ok_or("Не удалось определить IP в локальной сети")?;
    let url = format!("http://{ip}:{port}");
    // Прямая ссылка: скан камерой открывает мобильную страницу, токен сохранится сам.
    let payload = format!("{url}/?token={token}");
    let code = qrcode::QrCode::new(payload.as_bytes()).map_err(|e| e.to_string())?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(220, 220)
        .dark_color(qrcode::render::svg::Color("#1a1a22"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    Ok(serde_json::json!({ "svg": svg, "url": url }))
}

/// Локальный IP: узнаём адрес исходящего интерфейса (пакеты никуда не шлются).
fn local_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

#[tauri::command]
pub fn setup_library(state: State<AppState>, path: String, mode: String) -> Result<(), String> {
    let root = PathBuf::from(&path);
    std::fs::create_dir_all(root.join("store")).map_err(|e| format!("Не удалось создать папку: {e}"))?;
    std::fs::create_dir_all(root.join("thumbs")).map_err(|e| e.to_string())?;

    let conn = db::open(&root.join("library.db")).map_err(|e| format!("Не удалось открыть базу: {e}"))?;
    *state.conn.lock().unwrap() = Some(conn);
    *state.root.lock().unwrap() = Some(root);

    let mut cfg = load_config();
    cfg.library_root = Some(path);
    cfg.mode = Some(mode);
    save_config(&cfg)
}

#[tauri::command]
pub fn set_mode(mode: String) -> Result<(), String> {
    let mut c = load_config();
    c.mode = Some(mode);
    save_config(&c)
}

// ---------- Импорт ----------

#[tauri::command]
pub async fn import_paths(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
    in_place: Option<bool>,
) -> Result<import::Summary, String> {
    let root = get_root(&state)?;
    let in_place = in_place.unwrap_or(false);
    let lossless = load_config().lossless.unwrap_or(false);
    let summary = tauri::async_runtime::spawn_blocking(move || {
        let inputs: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        // Ручной импорт: false — если пользователь сам добавляет удалённый ранее
        // арт, «надгробие» снимается.
        import::run(&root, &inputs, in_place, lossless, false, |p| {
            let _ = app.emit("import-progress", p);
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(summary)
}

// ---------- Отслеживаемые папки (watcher) ----------

#[tauri::command]
pub fn list_watched() -> Vec<WatchedFolder> {
    load_config().watched
}

#[tauri::command]
pub fn add_watched(
    app: AppHandle,
    ws: State<crate::watcher::WatchState>,
    path: String,
    in_place: bool,
) -> Result<(), String> {
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("Пустой путь".into());
    }
    let mut c = load_config();
    if c.watched.iter().any(|w| w.path == path) {
        return Err("Эта папка уже отслеживается".into());
    }
    c.watched.push(WatchedFolder {
        path: path.clone(),
        in_place,
    });
    save_config(&c)?;
    crate::watcher::watch_path(ws.inner(), &path);
    crate::watcher::initial_scan(app.clone(), path, in_place);
    Ok(())
}

#[tauri::command]
pub fn remove_watched(ws: State<crate::watcher::WatchState>, path: String) -> Result<(), String> {
    let mut c = load_config();
    c.watched.retain(|w| w.path != path);
    save_config(&c)?;
    crate::watcher::unwatch_path(ws.inner(), &path);
    Ok(())
}

// ---------- Галерея и поиск ----------

/// Разобранный поисковый запрос: SQL-фильтры (теги, типы) + свободный текст.
pub struct SearchFilters {
    pub sql: String,        // фрагменты " AND …" с плейсхолдерами ?
    pub binds: Vec<String>, // значения плейсхолдеров
    pub free: Vec<String>,  // слова без синтаксиса
}

pub(crate) fn build_filters(search: &str) -> SearchFilters {
    let mut sql = String::new();
    let mut binds: Vec<String> = Vec::new();
    let mut free: Vec<String> = Vec::new();

    for raw in search.split_whitespace() {
        let (neg, tok) = match raw.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, raw),
        };
        if tok.is_empty() {
            continue;
        }
        // Тип файла: "gif", "видео", "type:video", "тип:фото"…
        let kind_of = |t: &str| -> Option<&'static str> {
            match t {
                "gif" | "гиф" | "гифка" | "anim" | "animation" => Some("animation"),
                "video" | "видео" | "webm" | "mp4" => Some("video"),
                "image" | "photo" | "фото" | "картинка" | "пикча" => Some("image"),
                _ => None,
            }
        };
        let tl = tok.to_lowercase();
        let kind_val = if let Some((pre, val)) = tl.split_once(':') {
            if matches!(pre, "type" | "kind" | "тип") {
                kind_of(val)
            } else {
                None
            }
        } else {
            kind_of(&tl)
        };
        if let Some(k) = kind_val {
            sql.push_str(if neg { " AND NOT f.kind = ?" } else { " AND f.kind = ?" });
            binds.push(k.to_string());
            continue;
        }

        // Здоровье библиотеки: арты без тегов / без коллекций
        let health = match tl.as_str() {
            "untagged" | "type:untagged" | "безтегов" => Some("untagged"),
            "uncollected" | "type:uncollected" => Some("uncollected"),
            _ => None,
        };
        if let Some(h) = health {
            let cond = if h == "untagged" {
                "NOT EXISTS (SELECT 1 FROM file_tags ftu WHERE ftu.file_id = f.id)"
            } else {
                "NOT EXISTS (SELECT 1 FROM collection_files cfu WHERE cfu.file_id = f.id)"
            };
            sql.push_str(if neg { " AND NOT (" } else { " AND (" });
            sql.push_str(cond);
            sql.push(')');
            continue;
        }

        // Фильтр по группам: type:group (все стопки), type:comic, type:variants.
        // Голые слова «группы»/«стопки» тоже работают — как gif/видео.
        let group_kind = |t: &str| -> Option<&'static str> {
            match t {
                "group" | "groups" | "группа" | "группы" | "стопка" | "стопки" => Some("any"),
                "comic" | "comics" | "комикс" | "комиксы" => Some("comic"),
                "variants" | "вариации" | "варианты" => Some("variants"),
                _ => None,
            }
        };
        let grp_val = if let Some((pre, val)) = tl.split_once(':') {
            if matches!(pre, "type" | "kind" | "тип") {
                group_kind(val)
            } else {
                None
            }
        } else {
            // Без префикса — только «общие» синонимы, чтобы не красть слова у тегов
            match tl.as_str() {
                "group" | "группа" | "группы" | "стопка" | "стопки" => Some("any"),
                _ => None,
            }
        };
        if let Some(gk) = grp_val {
            let cond = match gk {
                "any" => "f.group_id IS NOT NULL".to_string(),
                k => format!("f.group_id IN (SELECT id FROM groups WHERE kind = '{k}')"),
            };
            sql.push_str(if neg { " AND NOT (" } else { " AND (" });
            sql.push_str(&cond);
            sql.push(')');
            continue;
        }
        if let Some((ns, name)) = tok.split_once(':') {
            // Точный неймспейс + подстрока имени: species:fox
            sql.push_str(if neg { " AND NOT " } else { " AND " });
            sql.push_str(
                "EXISTS (SELECT 1 FROM file_tags ft JOIN tags tg ON tg.id = ft.tag_id \
                 WHERE ft.file_id = f.id AND tg.namespace = ? AND tg.name LIKE ?)",
            );
            binds.push(ns.to_lowercase());
            binds.push(format!("%{}%", name.to_lowercase()));
        } else if neg {
            sql.push_str(
                " AND NOT EXISTS (SELECT 1 FROM file_tags ft JOIN tags tg ON tg.id = ft.tag_id \
                 WHERE ft.file_id = f.id AND tg.name LIKE ?)",
            );
            binds.push(format!("%{}%", tok.to_lowercase()));
        } else {
            free.push(tok.to_lowercase());
        }
    }
    SearchFilters { sql, binds, free }
}

/// SQL-условие «файл NSFW?»: ручная пометка или rating-тег от ИИ.
pub(crate) fn nsfw_clause(want: i64) -> String {
    format!(
        " AND COALESCE(f.nsfw, (CASE WHEN EXISTS (SELECT 1 FROM file_tags ftn \
         JOIN tags tgn ON tgn.id = ftn.tag_id WHERE ftn.file_id = f.id \
         AND tgn.namespace = 'rating' AND tgn.name IN ('explicit','questionable')) \
         THEN 1 ELSE 0 END)) = {}",
        want.clamp(0, 1)
    )
}

// ---------- Массовое редактирование тегов ----------

#[derive(Serialize)]
pub struct TagRow {
    pub id: i64,
    pub namespace: String,
    pub name: String,
    pub count: i64,
    pub manual: i64, // сколько раз проставлен вручную (остальное — автотеги)
}

/// Все теги библиотеки с частотой (для менеджера тегов).
#[tauri::command]
pub fn list_all_tags(state: State<AppState>, search: String) -> Result<Vec<TagRow>, String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let q = search.trim().to_lowercase();
    let like = format!("%{q}%");
    let mut stmt = conn
        .prepare(
            "SELECT tg.id, tg.namespace, tg.name, COUNT(ft.file_id), \
             COALESCE(SUM(CASE WHEN ft.origin = 'manual' THEN 1 ELSE 0 END), 0) \
             FROM tags tg LEFT JOIN file_tags ft ON ft.tag_id = tg.id \
             WHERE (?1 = '' OR tg.name LIKE ?2 OR tg.namespace LIKE ?2) \
             GROUP BY tg.id ORDER BY COUNT(ft.file_id) DESC, tg.name LIMIT 500",
        )
        .map_err(|e| e.to_string())?;
    let out: Vec<TagRow> = stmt
        .query_map(params![q, like], |r| {
            Ok(TagRow {
                id: r.get(0)?,
                namespace: r.get(1)?,
                name: r.get(2)?,
                count: r.get(3)?,
                manual: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    Ok(out)
}

/// Переименование тега во всей библиотеке. Если такой тег уже есть — сливаем.
#[tauri::command]
pub fn tag_rename(
    state: State<AppState>,
    id: i64,
    namespace: String,
    name: String,
) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let ns = namespace.trim().to_lowercase();
    let nm = name.trim().to_lowercase();
    if nm.is_empty() {
        return Err("Пустое имя тега".into());
    }
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM tags WHERE namespace = ?1 AND name = ?2 AND id <> ?3",
            params![ns, nm, id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    match existing {
        // Такой тег уже существует — это слияние
        Some(target) => merge_tags(conn, id, target),
        None => {
            conn.execute(
                "UPDATE tags SET namespace = ?1, name = ?2 WHERE id = ?3",
                params![ns, nm, id],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        }
    }
}

fn merge_tags(conn: &Connection, from: i64, to: i64) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    // Переносим связи с файлами (дубли игнорируем), затем чистим исходный тег
    conn.execute(
        "INSERT OR IGNORE INTO file_tags (file_id, tag_id, origin, confidence) \
         SELECT file_id, ?1, origin, confidence FROM file_tags WHERE tag_id = ?2",
        params![to, from],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM file_tags WHERE tag_id = ?1", [from])
        .map_err(|e| e.to_string())?;
    // Связи implies/alias тоже переносим
    let _ = conn.execute(
        "UPDATE OR IGNORE tag_relations SET parent_id = ?1 WHERE parent_id = ?2",
        params![to, from],
    );
    let _ = conn.execute(
        "UPDATE OR IGNORE tag_relations SET child_id = ?1 WHERE child_id = ?2",
        params![to, from],
    );
    let _ = conn.execute(
        "DELETE FROM tag_relations WHERE parent_id = child_id",
        [],
    );
    conn.execute("DELETE FROM tags WHERE id = ?1", [from])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Слить несколько тегов в один (target остаётся).
#[tauri::command]
pub fn tags_merge(state: State<AppState>, ids: Vec<i64>, target: i64) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    for id in ids {
        merge_tags(conn, id, target)?;
    }
    Ok(())
}

/// Удалить теги из всей библиотеки. Возвращает, у скольких файлов сняты метки.
#[tauri::command]
pub fn tags_delete(state: State<AppState>, ids: Vec<i64>) -> Result<i64, String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let mut affected = 0i64;
    for id in ids {
        affected += conn
            .execute("DELETE FROM file_tags WHERE tag_id = ?1", [id])
            .map_err(|e| e.to_string())? as i64;
        conn.execute("DELETE FROM tags WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
    }
    Ok(affected)
}

// ---------- Связи тегов (implies / alias) ----------

/// Разбирает "ns:name" или "name" → (namespace, name) в нижнем регистре.
fn parse_tag_ref(raw: &str) -> (String, String) {
    let t = raw.trim();
    if let Some((ns, name)) = t.split_once(':') {
        (ns.trim().to_lowercase(), name.trim().to_lowercase())
    } else {
        (String::new(), t.to_lowercase())
    }
}

/// Разворачивает набор тегов по связям: добавляет антецеденты implies
/// (поиск следствия находит и специфичные теги) и обе стороны alias.
fn expand_tag_ids(conn: &Connection, seeds: &[i64]) -> Vec<i64> {
    if seeds.is_empty() {
        return Vec::new();
    }
    let rels: Vec<(i64, i64, String)> = {
        let mut stmt = match conn.prepare("SELECT parent_id, child_id, kind FROM tag_relations") {
            Ok(s) => s,
            Err(_) => return seeds.to_vec(),
        };
        let r = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>());
        match r {
            Ok(v) => v,
            Err(_) => return seeds.to_vec(),
        }
    };
    let mut set: std::collections::HashSet<i64> = seeds.iter().copied().collect();
    let mut changed = true;
    let mut iters = 0;
    while changed && iters < 50 {
        changed = false;
        iters += 1;
        for (p, c, kind) in &rels {
            match kind.as_str() {
                // p подразумевает c: поиск c должен находить и p
                "implies" => {
                    if set.contains(c) && set.insert(*p) {
                        changed = true;
                    }
                }
                // синонимы — в обе стороны
                "alias" => {
                    if set.contains(p) && set.insert(*c) {
                        changed = true;
                    }
                    if set.contains(c) && set.insert(*p) {
                        changed = true;
                    }
                }
                _ => {}
            }
        }
    }
    set.into_iter().collect()
}

#[derive(Serialize)]
pub struct TagRelationOut {
    pub id: i64,
    pub kind: String,
    pub from: String, // parent / антецедент ("ns:name" или "name")
    pub to: String,   // child / следствие
}

#[tauri::command]
pub fn add_tag_relation(
    state: State<AppState>,
    from: String,
    to: String,
    kind: String,
) -> Result<(), String> {
    let kind = kind.trim().to_lowercase();
    if kind != "implies" && kind != "alias" {
        return Err("Тип связи должен быть implies или alias".into());
    }
    let (fns, fname) = parse_tag_ref(&from);
    let (tns, tname) = parse_tag_ref(&to);
    if fname.is_empty() || tname.is_empty() {
        return Err("Укажи оба тега".into());
    }
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let from_id = upsert_tag(conn, &fns, &fname)?;
    let to_id = upsert_tag(conn, &tns, &tname)?;
    if from_id == to_id {
        return Err("Нельзя связать тег сам с собой".into());
    }
    conn.execute(
        "INSERT OR IGNORE INTO tag_relations (parent_id, child_id, kind) VALUES (?1, ?2, ?3)",
        params![from_id, to_id, kind],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn list_tag_relations(state: State<AppState>) -> Result<Vec<TagRelationOut>, String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let mut stmt = conn
        .prepare(
            "SELECT r.id, r.kind, p.namespace, p.name, c.namespace, c.name \
             FROM tag_relations r \
             JOIN tags p ON p.id = r.parent_id \
             JOIN tags c ON c.id = r.child_id \
             ORDER BY r.kind, p.name",
        )
        .map_err(|e| e.to_string())?;
    let out = stmt
        .query_map([], |r| {
            let fmt = |ns: String, nm: String| if ns.is_empty() { nm } else { format!("{ns}:{nm}") };
            Ok(TagRelationOut {
                id: r.get(0)?,
                kind: r.get(1)?,
                from: fmt(r.get(2)?, r.get(3)?),
                to: fmt(r.get(4)?, r.get(5)?),
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(out)
}

#[tauri::command]
pub fn delete_tag_relation(state: State<AppState>, id: i64) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    conn.execute("DELETE FROM tag_relations WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Общий WHERE-хвост для запросов по файлам (галерея и счётчик текущего вида).
fn build_files_where(
    conn: &Connection,
    search: &str,
    collection: Option<i64>,
    nsfw: Option<i64>,
) -> Result<(String, Vec<String>), String> {
    let mut sql = String::new();
    if let Some(cid) = collection {
        if cid == -1 {
            // Спец-значение: файлы вне всех коллекций (нераспределённые)
            sql.push_str(
                " AND NOT EXISTS (SELECT 1 FROM collection_files cf WHERE cf.file_id = f.id)",
            );
        } else {
            sql.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM collection_files cf \
                 WHERE cf.file_id = f.id AND cf.collection_id = {cid})"
            ));
        }
    }
    if let Some(n) = nsfw {
        sql.push_str(&nsfw_clause(n));
    }
    let filters = build_filters(search);
    sql.push_str(&filters.sql);
    let mut binds = filters.binds;

    for w in &filters.free {
        // Базовое условие: имя тега или автор/название источника содержит слово.
        let mut cond = String::from(
            " AND (EXISTS (SELECT 1 FROM file_tags ft JOIN tags tg ON tg.id = ft.tag_id \
             WHERE ft.file_id = f.id AND tg.name LIKE ?) \
             OR EXISTS (SELECT 1 FROM sources s WHERE s.file_id = f.id \
             AND (s.artist LIKE ? OR s.title LIKE ?)) \
             OR EXISTS (SELECT 1 FROM groups g WHERE g.id = f.group_id AND g.name LIKE ?)",
        );
        let pat = format!("%{w}%");
        binds.push(pat.clone());
        binds.push(pat.clone());
        binds.push(pat.clone());
        binds.push(pat);

        // Разворот синонимов/импликаций: по точному совпадению имени тега.
        let seeds: Vec<i64> = {
            let mut s = conn
                .prepare("SELECT id FROM tags WHERE name = ?1")
                .map_err(|e| e.to_string())?;
            let v: Vec<i64> = s
                .query_map([w.as_str()], |r| r.get::<_, i64>(0))
                .map_err(|e| e.to_string())?
                .flatten()
                .collect();
            v
        };
        let extra: Vec<i64> = expand_tag_ids(conn, &seeds)
            .into_iter()
            .filter(|id| !seeds.contains(id))
            .collect();
        if !extra.is_empty() {
            let list = extra.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
            cond.push_str(&format!(
                " OR EXISTS (SELECT 1 FROM file_tags ftx WHERE ftx.file_id = f.id \
                 AND ftx.tag_id IN ({list}))"
            ));
        }
        cond.push(')');
        sql.push_str(&cond);
    }
    Ok((sql, binds))
}

/// Количество артов в текущем виде (фильтры/коллекция/поиск) — для строки статуса.
#[tauri::command]
pub fn count_files(
    state: State<AppState>,
    search: String,
    collection: Option<i64>,
    nsfw: Option<i64>,
    recent: Option<bool>,
) -> Result<i64, String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let (mut where_sql, binds) = build_files_where(conn, &search, collection, nsfw)?;
    if recent.unwrap_or(false) {
        where_sql.push_str(" AND f.viewed_at IS NOT NULL");
    }
    conn.query_row(
        &format!("SELECT COUNT(*) FROM files f WHERE 1=1{where_sql}"),
        params_from_iter(binds.iter()),
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

/// Статистика и «здоровье» библиотеки: топы, рост по месяцам и проблемные места.
#[tauri::command]
pub fn library_health(state: State<AppState>) -> Result<serde_json::Value, String> {
    let root = get_root(&state)?;
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;

    let one = |sql: &str| -> i64 {
        conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0)
    };

    let files = one("SELECT COUNT(*) FROM files");
    let untagged = one(
        "SELECT COUNT(*) FROM files f WHERE NOT EXISTS \
         (SELECT 1 FROM file_tags ft WHERE ft.file_id = f.id)",
    );
    let uncollected = one(
        "SELECT COUNT(*) FROM files f WHERE NOT EXISTS \
         (SELECT 1 FROM collection_files cf WHERE cf.file_id = f.id)",
    );
    let unindexed = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM files f LEFT JOIN clip_vectors v ON v.file_id = f.id \
                 WHERE {AI_PENDING_WHERE}"
            ),
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0);
    let no_thumb = one("SELECT COUNT(*) FROM files WHERE thumb_rel IS NULL");
    let groups = one("SELECT COUNT(*) FROM groups");
    let grouped = one("SELECT COUNT(*) FROM files WHERE group_id IS NOT NULL");
    let collections = one("SELECT COUNT(*) FROM collections");
    let tags_total = one("SELECT COUNT(*) FROM tags");
    let viewed = one("SELECT COUNT(*) FROM files WHERE viewed_at IS NOT NULL");

    // Битые in-place: файл-оригинал пропал с диска
    let mut broken: Vec<i64> = Vec::new();
    {
        let mut s = conn
            .prepare("SELECT id, rel_path FROM files WHERE storage = 'inplace'")
            .map_err(|e| e.to_string())?;
        let rows: Vec<(i64, String)> = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        for (id, rel) in rows {
            if !root.join(&rel).exists() {
                broken.push(id);
            }
        }
    }

    let list = |sql: &str| -> Vec<serde_json::Value> {
        let Ok(mut s) = conn.prepare(sql) else { return Vec::new() };
        s.query_map([], |r| {
            Ok(serde_json::json!({
                "name": r.get::<_, String>(0)?,
                "count": r.get::<_, i64>(1)?
            }))
        })
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
    };

    let top_tags = list(
        "SELECT tg.name, COUNT(*) c FROM file_tags ft JOIN tags tg ON tg.id = ft.tag_id \
         WHERE tg.namespace <> 'rating' GROUP BY tg.id ORDER BY c DESC LIMIT 15",
    );
    let top_artists = list(
        "SELECT s.artist, COUNT(DISTINCT s.file_id) c FROM sources s \
         WHERE s.artist IS NOT NULL AND TRIM(s.artist) <> '' \
         GROUP BY s.artist ORDER BY c DESC LIMIT 15",
    );
    let by_month = list(
        "SELECT strftime('%Y-%m', imported_at, 'unixepoch') m, COUNT(*) c FROM files \
         GROUP BY m ORDER BY m DESC LIMIT 12",
    );
    let by_kind = list("SELECT kind, COUNT(*) c FROM files GROUP BY kind ORDER BY c DESC");

    Ok(serde_json::json!({
        "files": files,
        "collections": collections,
        "groups": groups,
        "grouped": grouped,
        "tags_total": tags_total,
        "viewed": viewed,
        "untagged": untagged,
        "uncollected": uncollected,
        "unindexed": unindexed,
        "no_thumb": no_thumb,
        "broken_inplace": broken.len() as i64,
        "top_tags": top_tags,
        "top_artists": top_artists,
        "by_month": by_month,
        "by_kind": by_kind,
    }))
}

/// Отметить арт просмотренным (история просмотров).
#[tauri::command]
pub fn mark_viewed(state: State<AppState>, file_id: i64) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    conn.execute(
        "UPDATE files SET viewed_at = ?1, view_count = COALESCE(view_count, 0) + 1 WHERE id = ?2",
        params![unix_now(), file_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Очистить историю просмотров.
#[tauri::command]
pub fn clear_view_history(state: State<AppState>) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    conn.execute("UPDATE files SET viewed_at = NULL, view_count = 0", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn query_files(
    state: State<AppState>,
    search: String,
    offset: i64,
    limit: i64,
    collection: Option<i64>,
    nsfw: Option<i64>,
    recent: Option<bool>,
) -> Result<Vec<FileCard>, String> {
    let root = get_root(&state)?;
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;

    let (mut where_sql, binds) = build_files_where(conn, &search, collection, nsfw)?;
    let recent = recent.unwrap_or(false);
    if recent {
        where_sql.push_str(" AND f.viewed_at IS NOT NULL");
    }
    // В галерее группа выглядит одной «стопкой»: показываем только обложку.
    let mut sql =
        format!("SELECT {CARD_COLS} FROM files f WHERE 1=1{where_sql}{GROUP_COVER_WHERE}");

    let order = if recent {
        "f.viewed_at DESC, f.id DESC" // история просмотров — по свежести просмотра
    } else {
        "f.imported_at DESC, f.id DESC"
    };
    sql.push_str(&format!(
        " ORDER BY {order} LIMIT {} OFFSET {}",
        limit.clamp(1, 500),
        offset.max(0)
    ));

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let cards = stmt
        .query_map(params_from_iter(binds.iter()), |row| {
            card_from_row(&root, row)
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(cards)
}

// ---------- Детали файла и теги ----------

#[derive(Serialize)]
pub struct TagOut {
    pub id: i64,
    pub namespace: String,
    pub name: String,
    pub origin: String,
}

#[derive(Serialize)]
pub struct SourceOut {
    pub url: Option<String>,
    pub site: Option<String>,
    pub artist: Option<String>,
    pub title: Option<String>,
}

#[derive(Serialize)]
pub struct FileDetails {
    #[serde(flatten)]
    pub card: FileCard,
    pub sha256: String,
    pub tags: Vec<TagOut>,
    pub sources: Vec<SourceOut>,
    pub collections: Vec<i64>, // id коллекций, в которых состоит файл
}

#[tauri::command]
pub fn file_details(state: State<AppState>, id: i64) -> Result<FileDetails, String> {
    let root = get_root(&state)?;
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;

    let sql = format!("SELECT {CARD_COLS}, f.sha256 FROM files f WHERE f.id = ?1");
    let (card, sha256) = conn
        .query_row(&sql, [id], |row| {
            let card = card_from_row(&root, row)?;
            let sha: String = row.get(13)?; // после group-колонок (8–12)
            Ok((card, sha))
        })
        .map_err(|_| "Файл не найден".to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT tg.id, tg.namespace, tg.name, ft.origin FROM file_tags ft \
             JOIN tags tg ON tg.id = ft.tag_id WHERE ft.file_id = ?1 \
             ORDER BY tg.namespace, tg.name",
        )
        .map_err(|e| e.to_string())?;
    let tags = stmt
        .query_map([id], |r| {
            Ok(TagOut {
                id: r.get(0)?,
                namespace: r.get(1)?,
                name: r.get(2)?,
                origin: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare("SELECT url, site, artist, title FROM sources WHERE file_id = ?1")
        .map_err(|e| e.to_string())?;
    let sources = stmt
        .query_map([id], |r| {
            Ok(SourceOut {
                url: r.get(0)?,
                site: r.get(1)?,
                artist: r.get(2)?,
                title: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare("SELECT collection_id FROM collection_files WHERE file_id = ?1")
        .map_err(|e| e.to_string())?;
    let collections = stmt
        .query_map([id], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(FileDetails {
        card,
        sha256,
        tags,
        sources,
        collections,
    })
}

/// Создаёт тег (если нет) и возвращает его id.
fn upsert_tag(conn: &Connection, namespace: &str, name: &str) -> Result<i64, String> {
    let ns = namespace.trim().to_lowercase();
    let nm = name.trim().to_lowercase();
    if nm.is_empty() {
        return Err("Пустой тег".into());
    }
    conn.execute(
        "INSERT OR IGNORE INTO tags (namespace, name) VALUES (?1, ?2)",
        params![ns, nm],
    )
    .map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT id FROM tags WHERE namespace = ?1 AND name = ?2",
        params![ns, nm],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_tag(
    state: State<AppState>,
    file_id: i64,
    namespace: String,
    name: String,
) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let tag_id = upsert_tag(conn, &namespace, &name)?;
    conn.execute(
        "INSERT OR IGNORE INTO file_tags (file_id, tag_id, origin) VALUES (?1, ?2, 'manual')",
        params![file_id, tag_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Добавить один тег сразу нескольким файлам (мультивыбор).
#[tauri::command]
pub fn add_tag_many(
    state: State<AppState>,
    file_ids: Vec<i64>,
    namespace: String,
    name: String,
) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let tag_id = upsert_tag(conn, &namespace, &name)?;
    for fid in file_ids {
        conn.execute(
            "INSERT OR IGNORE INTO file_tags (file_id, tag_id, origin) VALUES (?1, ?2, 'manual')",
            params![fid, tag_id],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(Serialize)]
pub struct TagSuggestion {
    pub namespace: String,
    pub name: String,
    pub count: i64,
}

/// Автодополнение тегов по префиксу. Поддерживает "ns:pre" и просто "pre".
#[tauri::command]
pub fn suggest_tags(state: State<AppState>, prefix: String) -> Result<Vec<TagSuggestion>, String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;

    let p = prefix.trim().to_lowercase();
    if p.is_empty() {
        return Ok(vec![]);
    }
    let (ns, name) = match p.split_once(':') {
        Some((a, b)) => (Some(a.to_string()), b.to_string()),
        None => (None, p),
    };
    let like = format!("{name}%");

    let map_row = |r: &rusqlite::Row| -> rusqlite::Result<TagSuggestion> {
        Ok(TagSuggestion {
            namespace: r.get(0)?,
            name: r.get(1)?,
            count: r.get(2)?,
        })
    };

    let out = if let Some(ns) = ns {
        let mut stmt = conn
            .prepare(
                "SELECT tg.namespace, tg.name, COUNT(ft.file_id) AS cnt FROM tags tg \
                 LEFT JOIN file_tags ft ON ft.tag_id = tg.id \
                 WHERE tg.name LIKE ?1 AND tg.namespace = ?2 \
                 GROUP BY tg.id ORDER BY cnt DESC, tg.name LIMIT 10",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![like, ns], map_row)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    } else {
        let mut stmt = conn
            .prepare(
                "SELECT tg.namespace, tg.name, COUNT(ft.file_id) AS cnt FROM tags tg \
                 LEFT JOIN file_tags ft ON ft.tag_id = tg.id \
                 WHERE tg.name LIKE ?1 \
                 GROUP BY tg.id ORDER BY cnt DESC, tg.name LIMIT 10",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![like], map_row)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };
    Ok(out)
}

#[tauri::command]
pub fn remove_tag(state: State<AppState>, file_id: i64, tag_id: i64) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    conn.execute(
        "DELETE FROM file_tags WHERE file_id = ?1 AND tag_id = ?2",
        params![file_id, tag_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------- Похожие ----------

#[tauri::command]
pub fn similar_local_cmd(
    state: State<AppState>,
    file_id: i64,
    limit: Option<i64>,
) -> Result<Vec<similar::SimilarCard>, String> {
    let root = get_root(&state)?;
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;

    // v0.2: если файл проиндексирован CLIP'ом — похожие по содержанию/стилю
    let qblob: Option<Vec<u8>> = conn
        .query_row(
            "SELECT embedding FROM clip_vectors WHERE file_id = ?1",
            [file_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let lim = limit.unwrap_or(24).clamp(1, 500) as usize;
    if let Some(qb) = qblob {
        if let Some(mut qv) = blob_to_vec(&qb) {
            normalize(&mut qv);
            let ranked = rank_by_vector(conn, &root, &qv, Some(file_id), lim, "", &[])?;
            if !ranked.is_empty() {
                return Ok(ranked
                    .into_iter()
                    .map(|(c, s)| similar::SimilarCard {
                        id: c.id,
                        kind: c.kind,
                        ext: c.ext,
                        width: c.width,
                        height: c.height,
                        size: c.size,
                        path: c.path,
                        thumb: c.thumb,
                        distance: 0,
                        similarity: (s.clamp(0.0, 1.0) * 100.0) as u32,
                    })
                    .collect());
            }
        }
    }
    // фолбэк: perceptual hash (точные копии/ресейвы)
    similar::similar_local(conn, &root, file_id, lim)
}

#[tauri::command]
pub async fn similar_web(
    state: State<'_, AppState>,
    file_id: i64,
) -> Result<Vec<similar::WebMatch>, String> {
    // Достаём путь к превью (или оригиналу) и отпускаем замки до await.
    let img_path: PathBuf = {
        let root = get_root(&state)?;
        let guard = state.conn.lock().unwrap();
        let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
        let (rel, thumb, kind): (String, Option<String>, String) = conn
            .query_row(
                "SELECT rel_path, thumb_rel, kind FROM files WHERE id = ?1",
                [file_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|_| "Файл не найден".to_string())?;
        match thumb {
            Some(t) => root.join(t),
            None if kind == "image" => root.join(rel),
            None => return Err("Для видео без превью поиск в интернете недоступен".into()),
        }
    };
    similar::fluffle_search(img_path).await
}

/// Сохраняет превью видео, снятое фронтендом (canvas → JPEG base64).
/// Возвращает абсолютный путь к превью.
#[tauri::command]
pub fn save_video_thumb(
    state: State<AppState>,
    file_id: i64,
    data_b64: String,
) -> Result<String, String> {
    use base64::Engine;
    let root = get_root(&state)?;
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;

    let (sha, existing): (String, Option<String>) = conn
        .query_row(
            "SELECT sha256, thumb_rel FROM files WHERE id = ?1",
            [file_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| "Файл не найден".to_string())?;
    if let Some(t) = existing {
        return Ok(root.join(t).to_string_lossy().into_owned());
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.trim())
        .map_err(|e| format!("bad base64: {e}"))?;
    let rel = format!("thumbs/{}/{}.jpg", &sha[..2], sha);
    let abs = root.join(&rel);
    if let Some(dir) = abs.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&abs, bytes).map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE files SET thumb_rel = ?1 WHERE id = ?2",
        params![rel, file_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(abs.to_string_lossy().into_owned())
}

// ---------- Удаление ----------

fn delete_one(conn: &Connection, root: &Path, file_id: i64) -> Result<(), String> {
    let (rel, thumb, storage, sha): (String, Option<String>, Option<String>, String) = conn
        .query_row(
            "SELECT rel_path, thumb_rel, storage, sha256 FROM files WHERE id = ?1",
            [file_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|_| "Файл не найден".to_string())?;

    conn.execute("DELETE FROM files WHERE id = ?1", [file_id])
        .map_err(|e| e.to_string())?;

    // «Надгробие»: watcher не должен возвращать удалённое из отслеживаемых папок.
    let _ = conn.execute(
        "INSERT OR REPLACE INTO deleted_hashes (sha256, deleted_at) VALUES (?1, ?2)",
        params![sha, unix_now()],
    );

    // Оригинал in-place НЕ трогаем — убираем только копию из store.
    if storage.as_deref() != Some("inplace") {
        let _ = std::fs::remove_file(root.join(rel));
    }
    if let Some(t) = thumb {
        let _ = std::fs::remove_file(root.join(t));
    }
    Ok(())
}

/// Удаляет файл из библиотеки: запись в БД (теги/коллекции — каскадом)
/// и физические файлы в store/thumbs. Оригинал вне библиотеки не трогается.
#[tauri::command]
pub fn delete_file(state: State<AppState>, file_id: i64) -> Result<(), String> {
    let root = get_root(&state)?;
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    delete_one(conn, &root, file_id)
}

/// Удаление нескольких файлов (мультивыбор).
/// expand_groups = true (галерея): выбранная стопка удаляется целиком со всеми
/// страницами; false (просмотрщик) — только конкретная страница.
#[tauri::command]
pub fn delete_files(
    state: State<AppState>,
    file_ids: Vec<i64>,
    expand_groups: Option<bool>,
) -> Result<(), String> {
    let root = get_root(&state)?;
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let expand = expand_groups.unwrap_or(false);

    let mut all: Vec<i64> = Vec::new();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for fid in &file_ids {
        if expand {
            let gid: Option<i64> = conn
                .query_row("SELECT group_id FROM files WHERE id = ?1", [fid], |r| r.get(0))
                .optional()
                .map_err(|e| e.to_string())?
                .flatten();
            if let Some(g) = gid {
                let mut s = conn
                    .prepare("SELECT id FROM files WHERE group_id = ?1")
                    .map_err(|e| e.to_string())?;
                let members: Vec<i64> = s
                    .query_map([g], |r| r.get::<_, i64>(0))
                    .map_err(|e| e.to_string())?
                    .flatten()
                    .collect();
                for id in members {
                    if seen.insert(id) {
                        all.push(id);
                    }
                }
                continue;
            }
        }
        if seen.insert(*fid) {
            all.push(*fid);
        }
    }

    for fid in all {
        delete_one(conn, &root, fid)?;
    }
    // Осиротевшие группы (все страницы удалены) подчищаем
    let _ = conn.execute(
        "DELETE FROM groups WHERE id NOT IN \
         (SELECT DISTINCT group_id FROM files WHERE group_id IS NOT NULL)",
        [],
    );
    Ok(())
}

/// Добавить несколько файлов в коллекцию (мультивыбор, без переключения).
#[tauri::command]
pub fn collection_add_many(
    state: State<AppState>,
    collection_id: i64,
    file_ids: Vec<i64>,
) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    for fid in file_ids {
        conn.execute(
            "INSERT OR IGNORE INTO collection_files (collection_id, file_id, added_at) VALUES (?1, ?2, ?3)",
            params![collection_id, fid, unix_now()],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---------- Коллекции ----------

#[derive(Serialize)]
pub struct CollectionOut {
    pub id: i64,
    pub name: String,
    pub count: i64,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[tauri::command]
pub fn list_collections(state: State<AppState>) -> Result<Vec<CollectionOut>, String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let mut stmt = conn
        .prepare(
            "SELECT c.id, c.name, COUNT(cf.file_id) FROM collections c \
             LEFT JOIN collection_files cf ON cf.collection_id = c.id \
             GROUP BY c.id ORDER BY COALESCE(c.position, 999999), c.name",
        )
        .map_err(|e| e.to_string())?;
    let out = stmt
        .query_map([], |r| {
            Ok(CollectionOut {
                id: r.get(0)?,
                name: r.get(1)?,
                count: r.get(2)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(out)
}

#[tauri::command]
pub fn create_collection(state: State<AppState>, name: String) -> Result<i64, String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let nm = name.trim();
    if nm.is_empty() {
        return Err("Пустое имя коллекции".into());
    }
    conn.execute(
        "INSERT OR IGNORE INTO collections (name, created_at) VALUES (?1, ?2)",
        params![nm, unix_now()],
    )
    .map_err(|e| e.to_string())?;
    conn.query_row("SELECT id FROM collections WHERE name = ?1", [nm], |r| {
        r.get(0)
    })
    .map_err(|e| e.to_string())
}

/// Новый порядок коллекций: массив id в желаемой последовательности.
#[tauri::command]
pub fn reorder_collections(state: State<AppState>, ids: Vec<i64>) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    for (i, id) in ids.iter().enumerate() {
        conn.execute(
            "UPDATE collections SET position = ?1 WHERE id = ?2",
            params![i as i64, id],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn delete_collection(state: State<AppState>, id: i64) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    conn.execute("DELETE FROM collections WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Переименование коллекции.
#[tauri::command]
pub fn rename_collection(state: State<AppState>, id: i64, name: String) -> Result<(), String> {
    let nm = name.trim();
    if nm.is_empty() {
        return Err("Пустое имя коллекции".into());
    }
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    conn.execute(
        "UPDATE collections SET name = ?1 WHERE id = ?2",
        params![nm, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Убрать несколько файлов из коллекции (мультивыбор при открытом фильтре).
#[tauri::command]
pub fn collection_remove_many(
    state: State<AppState>,
    collection_id: i64,
    file_ids: Vec<i64>,
) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    for fid in file_ids {
        conn.execute(
            "DELETE FROM collection_files WHERE collection_id = ?1 AND file_id = ?2",
            params![collection_id, fid],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---------- Группы: комиксы (страницы по порядку) и вариации одного арта ----------

#[derive(Serialize)]
pub struct GroupOut {
    pub id: i64,
    pub name: String,
    pub kind: String, // comic | variants
    pub files: Vec<FileCard>,
}

#[tauri::command]
pub fn group_create(
    state: State<AppState>,
    file_ids: Vec<i64>,
    kind: String,
    name: Option<String>,
) -> Result<i64, String> {
    if file_ids.len() < 2 {
        return Err("В группе должно быть минимум два арта".into());
    }
    let kind = if kind == "comic" { "comic" } else { "variants" };
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    conn.execute(
        "INSERT INTO groups (name, kind, created_at) VALUES (?1, ?2, ?3)",
        params![name.unwrap_or_default().trim(), kind, unix_now()],
    )
    .map_err(|e| e.to_string())?;
    let gid = conn.last_insert_rowid();
    // Порядок = порядок переданных id (порядок выделения); поправить можно в редакторе
    for (i, fid) in file_ids.iter().enumerate() {
        conn.execute(
            "UPDATE files SET group_id = ?1, group_pos = ?2 WHERE id = ?3",
            params![gid, i as i64, fid],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(gid)
}

#[tauri::command]
pub fn group_get(state: State<AppState>, id: i64) -> Result<GroupOut, String> {
    let root = get_root(&state)?;
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let (name, kind): (String, String) = conn
        .query_row("SELECT name, kind FROM groups WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|_| "Группа не найдена".to_string())?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {CARD_COLS} FROM files f WHERE f.group_id = ?1 \
             ORDER BY COALESCE(f.group_pos, 0), f.id"
        ))
        .map_err(|e| e.to_string())?;
    let files: Vec<FileCard> = stmt
        .query_map([id], |row| card_from_row(&root, row))
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    Ok(GroupOut { id, name, kind, files })
}

/// Обновление группы: имя, тип и точный состав/порядок (ordered_ids).
/// Файлы группы, не вошедшие в список, из неё исключаются.
/// Если осталось < 2 файлов — группа распускается.
#[tauri::command]
pub fn group_update(
    state: State<AppState>,
    id: i64,
    name: String,
    kind: String,
    ordered_ids: Vec<i64>,
) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    if ordered_ids.len() < 2 {
        conn.execute(
            "UPDATE files SET group_id = NULL, group_pos = NULL WHERE group_id = ?1",
            [id],
        )
        .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM groups WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let kind = if kind == "comic" { "comic" } else { "variants" };
    conn.execute(
        "UPDATE groups SET name = ?1, kind = ?2 WHERE id = ?3",
        params![name.trim(), kind, id],
    )
    .map_err(|e| e.to_string())?;
    let current: Vec<i64> = {
        let mut s = conn
            .prepare("SELECT id FROM files WHERE group_id = ?1")
            .map_err(|e| e.to_string())?;
        let v: Vec<i64> = s
            .query_map([id], |r| r.get::<_, i64>(0))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        v
    };
    for (i, fid) in ordered_ids.iter().enumerate() {
        conn.execute(
            "UPDATE files SET group_pos = ?1 WHERE id = ?2 AND group_id = ?3",
            params![i as i64, fid, id],
        )
        .map_err(|e| e.to_string())?;
    }
    for fid in current.iter().filter(|f| !ordered_ids.contains(f)) {
        conn.execute(
            "UPDATE files SET group_id = NULL, group_pos = NULL WHERE id = ?1",
            [fid],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(Serialize)]
pub struct GroupListItem {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub count: i64,
    pub cover: Option<String>, // превью обложки (для меню «Добавить в группу»)
}

/// Список всех групп (для меню «Добавить в группу»).
#[tauri::command]
pub fn list_groups(state: State<AppState>) -> Result<Vec<GroupListItem>, String> {
    let root = get_root(&state)?;
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let mut stmt = conn
        .prepare(
            "SELECT g.id, g.name, g.kind, COUNT(f.id), \
             (SELECT COALESCE(f2.thumb_rel, f2.rel_path) FROM files f2 \
              WHERE f2.group_id = g.id ORDER BY COALESCE(f2.group_pos, 0), f2.id LIMIT 1) \
             FROM groups g \
             LEFT JOIN files f ON f.group_id = g.id \
             GROUP BY g.id ORDER BY g.name, g.id",
        )
        .map_err(|e| e.to_string())?;
    let out: Vec<GroupListItem> = stmt
        .query_map([], |r| {
            Ok(GroupListItem {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                count: r.get(3)?,
                cover: r
                    .get::<_, Option<String>>(4)?
                    .map(|t| root.join(t).to_string_lossy().into_owned()),
            })
        })
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    Ok(out)
}

/// Добавить арты в существующую группу (в конец). Уже сгруппированные — пропускаются.
#[tauri::command]
pub fn group_add_files(
    state: State<AppState>,
    id: i64,
    file_ids: Vec<i64>,
) -> Result<i64, String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let max_pos: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(group_pos), -1) FROM files WHERE group_id = ?1",
            [id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut added = 0i64;
    for (i, fid) in file_ids.iter().enumerate() {
        let n = conn
            .execute(
                "UPDATE files SET group_id = ?1, group_pos = ?2 \
                 WHERE id = ?3 AND group_id IS NULL",
                params![id, max_pos + 1 + i as i64, fid],
            )
            .map_err(|e| e.to_string())?;
        added += n as i64;
    }
    Ok(added)
}

/// Объединение нескольких групп в одну (первая — целевая): страницы остальных
/// добавляются в конец с сохранением их внутреннего порядка.
#[tauri::command]
pub fn groups_merge(state: State<AppState>, group_ids: Vec<i64>) -> Result<i64, String> {
    if group_ids.len() < 2 {
        return Err("Выбери минимум две группы".into());
    }
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let target = group_ids[0];
    let mut pos: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(group_pos), -1) FROM files WHERE group_id = ?1",
            [target],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    for gid in group_ids.iter().skip(1) {
        let files: Vec<i64> = {
            let mut s = conn
                .prepare(
                    "SELECT id FROM files WHERE group_id = ?1 \
                     ORDER BY COALESCE(group_pos, 0), id",
                )
                .map_err(|e| e.to_string())?;
            let v: Vec<i64> = s
                .query_map([gid], |r| r.get::<_, i64>(0))
                .map_err(|e| e.to_string())?
                .flatten()
                .collect();
            v
        };
        for fid in files {
            pos += 1;
            conn.execute(
                "UPDATE files SET group_id = ?1, group_pos = ?2 WHERE id = ?3",
                params![target, pos, fid],
            )
            .map_err(|e| e.to_string())?;
        }
        // Имя/тип: пустое имя цели заполняем именем присоединяемой; comic побеждает
        let (nm, kd): (String, String) = conn
            .query_row("SELECT name, kind FROM groups WHERE id = ?1", [gid], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap_or_default();
        if !nm.trim().is_empty() {
            let _ = conn.execute(
                "UPDATE groups SET name = ?1 WHERE id = ?2 AND TRIM(name) = ''",
                params![nm, target],
            );
        }
        if kd == "comic" {
            let _ = conn.execute("UPDATE groups SET kind = 'comic' WHERE id = ?1", [target]);
        }
        conn.execute("DELETE FROM groups WHERE id = ?1", [gid])
            .map_err(|e| e.to_string())?;
    }
    Ok(target)
}

#[tauri::command]
pub fn group_dissolve(state: State<AppState>, id: i64) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    conn.execute(
        "UPDATE files SET group_id = NULL, group_pos = NULL WHERE group_id = ?1",
        [id],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM groups WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Serialize)]
pub struct AutoGroupProposal {
    pub kind: String, // comic | variants
    pub name: String,
    pub file_ids: Vec<i64>,
    pub thumbs: Vec<String>, // абсолютные пути превью первых страниц (для модалки)
}

/// Автосбор групп из разбросанных артов (только предложения, без применения):
/// комиксы — по общему названию из источника (e621/FA title), вариации — по
/// близкому perceptual hash. Уже сгруппированные файлы не трогаются.
#[tauri::command]
pub fn auto_groups_preview(state: State<AppState>) -> Result<Vec<AutoGroupProposal>, String> {
    use std::collections::{HashMap, HashSet};
    let root = get_root(&state)?;
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;

    let mut proposals: Vec<AutoGroupProposal> = Vec::new();
    let mut taken: HashSet<i64> = HashSet::new();

    // 1) Комиксы: одинаковый непустой title у нескольких файлов вне групп
    let title_rows: Vec<(String, i64)> = {
        let mut s = conn
            .prepare(
                "SELECT TRIM(s.title), f.id FROM sources s JOIN files f ON f.id = s.file_id \
                 WHERE f.group_id IS NULL AND s.title IS NOT NULL AND TRIM(s.title) <> '' \
                 ORDER BY f.id",
            )
            .map_err(|e| e.to_string())?;
        let v: Vec<(String, i64)> = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        v
    };
    let mut by_title: HashMap<String, Vec<i64>> = HashMap::new();
    for (title, fid) in title_rows {
        let ids = by_title.entry(title).or_default();
        if !ids.contains(&fid) {
            ids.push(fid);
        }
    }
    let mut comics: Vec<(String, Vec<i64>)> = by_title
        .into_iter()
        .filter(|(_, ids)| ids.len() >= 2 && ids.len() <= 300)
        .collect();
    comics.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    for (title, ids) in comics {
        for id in &ids {
            taken.insert(*id);
        }
        proposals.push(AutoGroupProposal {
            kind: "comic".into(),
            name: title,
            file_ids: ids, // порядок по id ≈ порядок импорта; правится в редакторе
            thumbs: Vec::new(),
        });
    }

    // 2) Вариации: near-dupe по phash среди оставшихся (порог мягче «Дубликатов»)
    const VAR_THRESHOLD: u32 = 12;
    let rows: Vec<(i64, i64)> = {
        let mut s = conn
            .prepare(
                "SELECT id, phash FROM files \
                 WHERE group_id IS NULL AND phash IS NOT NULL",
            )
            .map_err(|e| e.to_string())?;
        let v: Vec<(i64, i64)> = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        v
    };
    let rows: Vec<(i64, i64)> = rows.into_iter().filter(|(id, _)| !taken.contains(id)).collect();
    let n = rows.len();
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for j in (i + 1)..n {
            let d = ((rows[i].1 as u64) ^ (rows[j].1 as u64)).count_ones();
            if d <= VAR_THRESHOLD {
                let (a, b) = (uf_find(&mut parent, i), uf_find(&mut parent, j));
                if a != b {
                    parent[a] = b;
                }
            }
        }
    }
    let mut clusters: HashMap<usize, Vec<i64>> = HashMap::new();
    for i in 0..n {
        let r = uf_find(&mut parent, i);
        clusters.entry(r).or_default().push(rows[i].0);
    }
    let mut vars: Vec<Vec<i64>> = clusters
        .into_values()
        .filter(|ids| ids.len() >= 2 && ids.len() <= 30)
        .collect();
    vars.sort_by(|a, b| b.len().cmp(&a.len()));
    for ids in vars {
        proposals.push(AutoGroupProposal {
            kind: "variants".into(),
            name: String::new(),
            file_ids: ids,
            thumbs: Vec::new(),
        });
    }

    proposals.truncate(100); // не заваливаем UI

    // Превью всех страниц — в модалке можно отсечь лишний арт кликом
    for p in &mut proposals {
        for fid in p.file_ids.iter() {
            if let Ok(rel) = conn.query_row(
                "SELECT COALESCE(thumb_rel, rel_path) FROM files WHERE id = ?1",
                [fid],
                |r| r.get::<_, String>(0),
            ) {
                p.thumbs.push(root.join(rel).to_string_lossy().into_owned());
            }
        }
    }
    Ok(proposals)
}

/// Ищет имя комикса на e621 по первой странице группы (поиск по md5 → pool name).
/// Явное сетевое действие пользователя.
#[tauri::command]
pub async fn comic_name_from_e621(
    state: State<'_, AppState>,
    file_id: i64,
) -> Result<Option<String>, String> {
    let abs = {
        let root = get_root(&state)?;
        let guard = state.conn.lock().unwrap();
        let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
        let rel: String = conn
            .query_row("SELECT rel_path FROM files WHERE id = ?1", [file_id], |r| r.get(0))
            .map_err(|_| "Файл не найден".to_string())?;
        root.join(rel)
    };
    let bytes = std::fs::read(&abs).map_err(|e| e.to_string())?;
    let digest = {
        use md5::Digest;
        let mut h = md5::Md5::new();
        h.update(&bytes);
        hex::encode(h.finalize())
    };

    const UA: &str = "FurryManager/0.4 (local reference manager)";
    let client = reqwest::Client::new();
    let v: serde_json::Value = client
        .get(format!("https://e621.net/posts.json?tags=md5:{digest}"))
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("Нет соединения с e621: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Некорректный ответ e621: {e}"))?;

    let Some(post) = v
        .get("posts")
        .and_then(|p| p.as_array())
        .and_then(|a| a.first())
    else {
        return Ok(None); // на e621 этот арт не нашёлся
    };
    let Some(pool_id) = post
        .get("pools")
        .and_then(|p| p.as_array())
        .and_then(|a| a.first())
        .and_then(|x| x.as_i64())
    else {
        return Ok(None); // пост есть, но он не в пуле (не комикс)
    };
    let pv: serde_json::Value = client
        .get(format!("https://e621.net/pools.json?search[id]={pool_id}"))
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("Нет соединения с e621: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Некорректный ответ e621: {e}"))?;
    Ok(pv
        .as_array()
        .and_then(|a| a.first())
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.replace('_', " ")))
}

/// Добавляет файл в коллекцию или убирает, если уже там. Возвращает true, если добавлен.
#[tauri::command]
pub fn toggle_collection(
    state: State<AppState>,
    collection_id: i64,
    file_id: i64,
) -> Result<bool, String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM collection_files WHERE collection_id = ?1 AND file_id = ?2",
            params![collection_id, file_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if exists.is_some() {
        conn.execute(
            "DELETE FROM collection_files WHERE collection_id = ?1 AND file_id = ?2",
            params![collection_id, file_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(false)
    } else {
        conn.execute(
            "INSERT INTO collection_files (collection_id, file_id, added_at) VALUES (?1, ?2, ?3)",
            params![collection_id, file_id, unix_now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(true)
    }
}

// ---------- ИИ (v0.2): CLIP-поиск и автотеги ----------

pub struct AiState {
    pub sidecar: std::sync::Arc<crate::sidecar::Sidecar>,
    pub indexing: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Собранный PyInstaller'ом бинарник сайдкара (если есть — Python не нужен).
/// Ищем рядом с exe и в дереве проекта (dev-запуск).
pub fn packaged_sidecar() -> Option<PathBuf> {
    let name = if cfg!(windows) { "fm-sidecar.exe" } else { "fm-sidecar" };
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(dir.join(name));
            cands.push(dir.join("sidecar").join(name));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        // Установленное приложение: ресурсы Tauri лежат рядом с exe
        if let Some(dir) = exe.parent() {
            cands.push(dir.join("resources").join("sidecar").join(name));
            cands.push(dir.join("resources").join(name));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        cands.push(cwd.join("../sidecar/dist").join(name));
        cands.push(cwd.join("sidecar/dist").join(name));
        cands.push(cwd.join("../dist").join(name));
    }
    cands.into_iter().find(|p| p.exists())
}

/// Где брать сайдкар: (путь к скрипту ИЛИ к бинарнику, интерпретатор).
/// Если найден упакованный бинарник — python не используется (пустая строка).
fn sidecar_cfg() -> Result<(PathBuf, String), String> {
    if let Some(bin) = packaged_sidecar() {
        return Ok((bin, String::new()));
    }
    let cfg = load_config();
    let python = cfg.python_path.unwrap_or_else(|| "python".into());
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        cands.push(cwd.join("../sidecar/main.py"));
        cands.push(cwd.join("sidecar/main.py"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(dir.join("sidecar/main.py"));
            cands.push(dir.join("resources/sidecar/main.py"));
        }
    }
    for c in &cands {
        if c.exists() {
            return Ok((c.clone(), python));
        }
    }
    Err("Не найден ни собранный сайдкар (fm-sidecar), ни sidecar/main.py".into())
}

/// Статус ИИ-модуля для настроек.
#[tauri::command]
pub fn sidecar_status() -> serde_json::Value {
    match packaged_sidecar() {
        Some(p) => serde_json::json!({
            "packaged": true,
            "path": p.to_string_lossy(),
            "text": "Собранный сайдкар найден — Python не требуется."
        }),
        None => serde_json::json!({
            "packaged": false,
            "path": null,
            "text": "Работает через Python (sidecar/main.py). Можно собрать автономный fm-sidecar — см. sidecar/BUILD.md."
        }),
    }
}

fn blob_to_vec(b: &[u8]) -> Option<Vec<f32>> {
    if b.is_empty() || b.len() % 4 != 0 {
        return None;
    }
    Some(
        b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn json_to_vec(v: &serde_json::Value) -> Option<Vec<f32>> {
    let arr = v.as_array()?;
    let mut out: Vec<f32> = arr.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect();
    if out.is_empty() {
        return None;
    }
    normalize(&mut out);
    Some(out)
}

/// Ранжирует проиндексированные файлы по косинусной близости к qvec.
/// extra_sql/extra_binds — дополнительные SQL-фильтры (" AND …" c ?).
fn rank_by_vector(
    conn: &Connection,
    root: &Path,
    qvec: &[f32],
    exclude: Option<i64>,
    limit: usize,
    extra_sql: &str,
    extra_binds: &[String],
) -> Result<Vec<(FileCard, f32)>, String> {
    let sql = format!(
        "SELECT {CARD_COLS}, v.embedding FROM files f JOIN clip_vectors v ON v.file_id = f.id WHERE 1=1{extra_sql}"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params_from_iter(extra_binds.iter()), |row| {
            let card = card_from_row(root, row)?;
            let blob: Vec<u8> = row.get(13)?; // после group-колонок (8–12)
            Ok((card, blob))
        })
        .map_err(|e| e.to_string())?;

    let mut scored: Vec<(FileCard, f32)> = Vec::new();
    for r in rows.flatten() {
        let (card, blob) = r;
        if exclude == Some(card.id) {
            continue;
        }
        if let Some(v) = blob_to_vec(&blob) {
            if v.len() == qvec.len() {
                scored.push((card, dot(qvec, &v)));
            }
        }
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    Ok(scored)
}

/// Условие «файл нуждается в индексации»: нет вектора, прошлый раз не вышло
/// (пустой BLOB — например, файл был битым и потом починен), или теггер
/// тогда упал и не оставил ни одного wd14-тега.
/// Видео индексируются по превью-кадру (thumb_rel), поэтому попадают в очередь,
/// как только кадр снят (фронтендом или ffmpeg).
const AI_PENDING_WHERE: &str =
    "(f.kind IN ('image','animation') OR (f.kind = 'video' AND f.thumb_rel IS NOT NULL)) \
     AND (v.file_id IS NULL OR LENGTH(v.embedding) = 0 \
     OR NOT EXISTS (SELECT 1 FROM file_tags ftw JOIN tags tgw ON tgw.id = ftw.tag_id \
     WHERE ftw.file_id = f.id AND ftw.origin = 'wd14'))";

/// Сколько файлов ещё не проиндексировано нейросетью.
#[tauri::command]
pub fn ai_pending(state: State<AppState>) -> Result<i64, String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM files f LEFT JOIN clip_vectors v ON v.file_id = f.id \
             WHERE {AI_PENDING_WHERE}"
        ),
        [],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct ScoredCard {
    #[serde(flatten)]
    pub card: FileCard,
    pub score: f32, // косинусная близость 0..1
}

/// Остановить текущую индексацию (после текущей пачки).
#[tauri::command]
pub fn ai_cancel(ai: State<AiState>) {
    ai.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Умный поиск: текст → CLIP-вектор → ранжирование библиотеки.
#[tauri::command]
pub async fn semantic_search(
    state: State<'_, AppState>,
    ai: State<'_, AiState>,
    query: String,
    limit: i64,
    collection: Option<i64>,
    nsfw: Option<i64>,
) -> Result<Vec<ScoredCard>, String> {
    let q = query.trim().to_string();
    if q.is_empty() {
        return Ok(vec![]);
    }

    // Гибрид: тег-синтаксис и типы становятся SQL-фильтрами,
    // свободный текст — CLIP-ранжированием внутри отфильтрованного.
    let filters = build_filters(&q);
    let mut extra = String::new();
    if let Some(cid) = collection {
        if cid == -1 {
            extra.push_str(
                " AND NOT EXISTS (SELECT 1 FROM collection_files cf WHERE cf.file_id = f.id)",
            );
        } else {
            extra.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM collection_files cf \
                 WHERE cf.file_id = f.id AND cf.collection_id = {cid})"
            ));
        }
    }
    if let Some(n) = nsfw {
        extra.push_str(&nsfw_clause(n));
    }
    extra.push_str(&filters.sql);
    let free = filters.free.join(" ");

    // Только фильтры без свободного текста — хронологическая выдача (score = -1)
    if free.is_empty() {
        let root = get_root(&state)?;
        let guard = state.conn.lock().unwrap();
        let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
        let sql = format!(
            "SELECT {CARD_COLS} FROM files f WHERE 1=1{extra} \
             ORDER BY f.imported_at DESC, f.id DESC LIMIT {}",
            limit.clamp(1, 300)
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let cards = stmt
            .query_map(params_from_iter(filters.binds.iter()), |row| {
                card_from_row(&root, row)
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        return Ok(cards
            .into_iter()
            .map(|c| ScoredCard { card: c, score: -1.0 })
            .collect());
    }

    let (script, python) = sidecar_cfg()?;
    let sc = ai.sidecar.clone();
    let v = tauri::async_runtime::spawn_blocking(move || {
        sc.rpc(&script, &python, "embed_text", serde_json::json!({ "text": free }))
    })
    .await
    .map_err(|e| e.to_string())??;
    let qvec = json_to_vec(&v).ok_or("ИИ вернул некорректный вектор")?;

    let root = get_root(&state)?;
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let ranked = rank_by_vector(
        conn,
        &root,
        &qvec,
        None,
        limit.clamp(1, 300) as usize,
        &extra,
        &filters.binds,
    )?;
    Ok(ranked
        .into_iter()
        .filter(|(_, s)| *s > 0.15)
        .map(|(c, s)| ScoredCard { card: c, score: s })
        .collect())
}

fn add_wd_tag(conn: &Connection, file_id: i64, ns: &str, name: &str, score: f64) {
    if let Ok(tag_id) = upsert_tag(conn, ns, name) {
        let _ = conn.execute(
            "INSERT OR IGNORE INTO file_tags (file_id, tag_id, origin, confidence) \
             VALUES (?1, ?2, 'wd14', ?3)",
            params![file_id, tag_id, score],
        );
    }
}

/// Индексация: эмбеддинги + автотеги для всех необработанных файлов.
/// Прогресс — событие "index-progress". Возвращает число обработанных.
#[tauri::command]
pub async fn ai_index(
    app: AppHandle,
    state: State<'_, AppState>,
    ai: State<'_, AiState>,
) -> Result<i64, String> {
    use std::sync::atomic::Ordering;
    if ai.indexing.swap(true, Ordering::SeqCst) {
        return Err("Индексация уже идёт".into());
    }
    ai.cancel.store(false, Ordering::SeqCst);
    let res = ai_index_inner(&app, &state, &ai).await;
    ai.indexing.store(false, Ordering::SeqCst);
    res
}

async fn ai_index_inner(
    app: &AppHandle,
    state: &State<'_, AppState>,
    ai: &State<'_, AiState>,
) -> Result<i64, String> {
    let root = get_root(state)?;
    let (script, python) = sidecar_cfg()?;

    let total: i64 = {
        let guard = state.conn.lock().unwrap();
        let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
        conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM files f LEFT JOIN clip_vectors v ON v.file_id = f.id \
                 WHERE {AI_PENDING_WHERE}"
            ),
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?
    };
    if total == 0 {
        return Ok(0);
    }

    let mut done: i64 = 0;
    // Курсор по id: файл, у которого и после повтора нет тегов, не должен
    // выбираться снова в этом же запуске (иначе цикл никогда не закончится).
    let mut last_id: i64 = 0;
    loop {
        if ai.cancel.load(std::sync::atomic::Ordering::SeqCst) {
            break; // остановка по кнопке «Отменить»
        }
        // 1) Берём пачку необработанных (замок БД — только на время запроса)
        let batch: Vec<(i64, String, String, String)> = {
            let guard = state.conn.lock().unwrap();
            let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT f.id, COALESCE(f.thumb_rel, f.rel_path), f.kind, f.rel_path \
                     FROM files f \
                     LEFT JOIN clip_vectors v ON v.file_id = f.id \
                     WHERE f.id > ?1 AND {AI_PENDING_WHERE} \
                     ORDER BY f.id LIMIT 8"
                ))
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([last_id], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            rows
        };
        if batch.is_empty() {
            break;
        }
        last_id = batch.last().map(|(id, _, _, _)| *id).unwrap_or(i64::MAX);

        // 2) Готовим кадры: видео тегируем по 3 кадрам (NSFW-сцены редко в начале),
        //    остальное — по одному изображению. ffmpeg нет — фолбэк на превью.
        let (items, flat): (Vec<(i64, usize, Vec<std::path::PathBuf>, bool)>, Vec<String>) = {
            let root2 = root.clone();
            let batch2 = batch.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let mut items: Vec<(i64, usize, Vec<std::path::PathBuf>, bool)> = Vec::new();
                let mut flat: Vec<String> = Vec::new();
                for (fid, rel, kind, orig_rel) in &batch2 {
                    let mut tmp: Vec<std::path::PathBuf> = Vec::new();
                    let mut multi_video = false;
                    let paths: Vec<String> = if kind == "video" {
                        let frames = import::video_frames(&root2.join(orig_rel), 3);
                        if frames.is_empty() {
                            // ffmpeg нет — кадры доснимет фронтенд (vframes останется NULL)
                            vec![root2.join(rel).to_string_lossy().into_owned()]
                        } else {
                            multi_video = true;
                            let p = frames.iter().map(|f| f.to_string_lossy().into_owned()).collect();
                            tmp = frames;
                            p
                        }
                    } else {
                        vec![root2.join(rel).to_string_lossy().into_owned()]
                    };
                    let n = paths.len();
                    flat.extend(paths);
                    items.push((*fid, n, tmp, multi_video));
                }
                (items, flat)
            })
            .await
            .map_err(|e| e.to_string())?
        };

        // 3) Сайдкар: эмбеддинги (обязательно) и теги (не критично)
        let (sc, s2, p2, paths2) = (ai.sidecar.clone(), script.clone(), python.clone(), flat.clone());
        let vecs = tauri::async_runtime::spawn_blocking(move || {
            sc.rpc(&s2, &p2, "embed_images", serde_json::json!({ "paths": paths2 }))
        })
        .await
        .map_err(|e| e.to_string())??;

        let (sc, s2, p2, paths2) = (ai.sidecar.clone(), script.clone(), python.clone(), flat.clone());
        let tags = tauri::async_runtime::spawn_blocking(move || {
            sc.rpc(&s2, &p2, "tag_images", serde_json::json!({ "paths": paths2 }))
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r)
        .unwrap_or(serde_json::Value::Null);

        // 4) Свёртка мультикадровых результатов и запись
        {
            let guard = state.conn.lock().unwrap();
            let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
            let mut off = 0usize;
            for (fid, cnt, tmp, multi_video) in &items {
                // Эмбеддинг: среднее по кадрам (нормализованное)
                let mut acc: Option<Vec<f32>> = None;
                let mut m = 0f32;
                for k in 0..*cnt {
                    if let Some(v) = vecs.get(off + k).and_then(json_to_vec) {
                        match &mut acc {
                            None => acc = Some(v),
                            Some(a) => {
                                for (x, y) in a.iter_mut().zip(v) {
                                    *x += y;
                                }
                            }
                        }
                        m += 1.0;
                    }
                }
                let blob = acc
                    .map(|mut v| {
                        for x in v.iter_mut() {
                            *x /= m;
                        }
                        normalize(&mut v);
                        vec_to_blob(&v)
                    })
                    .unwrap_or_default(); // пустой BLOB = «не удалось», чтобы не зациклиться
                conn.execute(
                    "INSERT OR REPLACE INTO clip_vectors (file_id, embedding) VALUES (?1, ?2)",
                    params![fid, blob],
                )
                .map_err(|e| e.to_string())?;

                // Теги: max score по (категория, имя) среди всех кадров —
                // NSFW-кадр из середины видео не потеряется
                let mut merged: std::collections::HashMap<(i64, String), f64> =
                    std::collections::HashMap::new();
                for k in 0..*cnt {
                    if let Some(arr) = tags.get(off + k).and_then(|t| t.as_array()) {
                        for it in arr {
                            let name = it.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let cat = it.get("category").and_then(|c| c.as_i64()).unwrap_or(-1);
                            let score = it.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
                            if name.is_empty() {
                                continue;
                            }
                            let e = merged.entry((cat, name.to_string())).or_insert(0.0);
                            if score > *e {
                                *e = score;
                            }
                        }
                    }
                }
                let mut best_rating: Option<(String, f64)> = None;
                for ((cat, name), score) in &merged {
                    let pretty = name.replace('_', " ");
                    match cat {
                        0 if *score >= 0.35 => add_wd_tag(conn, *fid, "", &pretty, *score),
                        3 if *score >= 0.85 => add_wd_tag(conn, *fid, "series", &pretty, *score),
                        4 if *score >= 0.85 => add_wd_tag(conn, *fid, "character", &pretty, *score),
                        9 => {
                            if best_rating.as_ref().map(|(_, s)| *score > *s).unwrap_or(true) {
                                best_rating = Some((pretty.clone(), *score));
                            }
                        }
                        _ => {}
                    }
                }
                if let Some((r, s)) = best_rating {
                    add_wd_tag(conn, *fid, "rating", &r, s);
                }
                if *multi_video {
                    let _ = conn.execute("UPDATE files SET vframes = 1 WHERE id = ?1", [fid]);
                }

                for t in tmp {
                    let _ = std::fs::remove_file(t); // временные кадры
                }
                off += cnt;
            }
        }

        done += batch.len() as i64;
        let _ = app.emit("index-progress", serde_json::json!({ "done": done, "total": total }));
    }
    Ok(done)
}

/// Полная переиндексация: сброс векторов и wd14-автотегов (ручные теги не трогаем).
/// После этого ai_index пройдёт всю библиотеку заново.
#[tauri::command]
pub fn ai_reindex_reset(state: State<AppState>) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    conn.execute("DELETE FROM clip_vectors", [])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM file_tags WHERE origin = 'wd14'", [])
        .map_err(|e| e.to_string())?;
    conn.execute("UPDATE files SET vframes = NULL WHERE kind = 'video'", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Видео, которым нужна мультикадровая индексация фронтендом (нет ffmpeg на ПК).
#[tauri::command]
pub fn list_videos_for_frames(state: State<AppState>) -> Result<Vec<serde_json::Value>, String> {
    let root = get_root(&state)?;
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let mut stmt = conn
        .prepare(
            "SELECT id, rel_path FROM files \
             WHERE kind = 'video' AND (vframes IS NULL OR vframes = 0) \
             ORDER BY id LIMIT 300",
        )
        .map_err(|e| e.to_string())?;
    let out: Vec<serde_json::Value> = stmt
        .query_map([], |r| {
            let id: i64 = r.get(0)?;
            let rel: String = r.get(1)?;
            Ok(serde_json::json!({
                "id": id,
                "path": root.join(rel).to_string_lossy(),
            }))
        })
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    Ok(out)
}

/// Индексация видео по кадрам, снятым фронтендом (когда ffmpeg недоступен):
/// эмбеддинг = среднее по кадрам, теги = max score, старые wd14-теги заменяются.
#[tauri::command]
pub async fn ai_index_video(
    state: State<'_, AppState>,
    ai: State<'_, AiState>,
    file_id: i64,
    frames_b64: Vec<String>,
) -> Result<(), String> {
    use base64::Engine;
    if frames_b64.is_empty() {
        return Err("нет кадров".into());
    }
    let (script, python) = sidecar_cfg()?;

    // Кадры во временные файлы
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut paths: Vec<String> = Vec::new();
    let mut tmps: Vec<PathBuf> = Vec::new();
    for (i, b) in frames_b64.iter().enumerate() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b.trim())
            .map_err(|e| format!("битый кадр: {e}"))?;
        let mut t = std::env::temp_dir();
        t.push(format!("fm_vf_{ts}_{file_id}_{i}.jpg"));
        std::fs::write(&t, &bytes).map_err(|e| e.to_string())?;
        paths.push(t.to_string_lossy().into_owned());
        tmps.push(t);
    }

    let (sc, s2, p2, pp) = (ai.sidecar.clone(), script.clone(), python.clone(), paths.clone());
    let vecs = tauri::async_runtime::spawn_blocking(move || {
        sc.rpc(&s2, &p2, "embed_images", serde_json::json!({ "paths": pp }))
    })
    .await
    .map_err(|e| e.to_string())??;

    let (sc, s2, p2, pp) = (ai.sidecar.clone(), script, python, paths.clone());
    let tags = tauri::async_runtime::spawn_blocking(move || {
        sc.rpc(&s2, &p2, "tag_images", serde_json::json!({ "paths": pp }))
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r)
    .unwrap_or(serde_json::Value::Null);

    for t in &tmps {
        let _ = std::fs::remove_file(t);
    }

    // Свёртка (как в основной индексации)
    let mut acc: Option<Vec<f32>> = None;
    let mut m = 0f32;
    for k in 0..paths.len() {
        if let Some(v) = vecs.get(k).and_then(json_to_vec) {
            match &mut acc {
                None => acc = Some(v),
                Some(a) => {
                    for (x, y) in a.iter_mut().zip(v) {
                        *x += y;
                    }
                }
            }
            m += 1.0;
        }
    }
    let blob = acc
        .map(|mut v| {
            for x in v.iter_mut() {
                *x /= m;
            }
            normalize(&mut v);
            vec_to_blob(&v)
        })
        .unwrap_or_default();

    let mut merged: std::collections::HashMap<(i64, String), f64> = std::collections::HashMap::new();
    for k in 0..paths.len() {
        if let Some(arr) = tags.get(k).and_then(|t| t.as_array()) {
            for it in arr {
                let name = it.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let cat = it.get("category").and_then(|c| c.as_i64()).unwrap_or(-1);
                let score = it.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
                if name.is_empty() {
                    continue;
                }
                let e = merged.entry((cat, name.to_string())).or_insert(0.0);
                if score > *e {
                    *e = score;
                }
            }
        }
    }

    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    conn.execute(
        "INSERT OR REPLACE INTO clip_vectors (file_id, embedding) VALUES (?1, ?2)",
        params![file_id, blob],
    )
    .map_err(|e| e.to_string())?;
    // Старые теги от одного превью-кадра больше не актуальны
    conn.execute(
        "DELETE FROM file_tags WHERE file_id = ?1 AND origin = 'wd14'",
        [file_id],
    )
    .map_err(|e| e.to_string())?;
    let mut best_rating: Option<(String, f64)> = None;
    for ((cat, name), score) in &merged {
        let pretty = name.replace('_', " ");
        match cat {
            0 if *score >= 0.35 => add_wd_tag(conn, file_id, "", &pretty, *score),
            3 if *score >= 0.85 => add_wd_tag(conn, file_id, "series", &pretty, *score),
            4 if *score >= 0.85 => add_wd_tag(conn, file_id, "character", &pretty, *score),
            9 => {
                if best_rating.as_ref().map(|(_, s)| *score > *s).unwrap_or(true) {
                    best_rating = Some((pretty.clone(), *score));
                }
            }
            _ => {}
        }
    }
    if let Some((r, s)) = best_rating {
        add_wd_tag(conn, file_id, "rating", &r, s);
    }
    conn.execute("UPDATE files SET vframes = 1 WHERE id = ?1", [file_id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Сброс всех ручных SFW/NSFW-пометок: распределение снова по rating-тегам ИИ.
#[tauri::command]
pub fn nsfw_reset_all(state: State<AppState>) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    conn.execute("UPDATE files SET nsfw = NULL", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Ручная пометка SFW/NSFW: value = 0 (SFW), 1 (NSFW), None (авто по тегам).
#[tauri::command]
pub fn set_nsfw(
    state: State<AppState>,
    file_ids: Vec<i64>,
    value: Option<i64>,
) -> Result<(), String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    for fid in file_ids {
        conn.execute(
            "UPDATE files SET nsfw = ?1 WHERE id = ?2",
            params![value, fid],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Результат вставки картинки из буфера обмена.
#[derive(Serialize)]
pub struct PasteResult {
    pub imported: bool, // true — новый файл, false — дубликат
    pub file_id: i64,
}

/// Вставка арта из буфера обмена (data_b64 — base64 картинки из фронтенда).
/// Файлу присваиваются переданные коллекция и метка nsfw (свойства открытого вида).
#[tauri::command]
pub fn import_clipboard(
    state: State<AppState>,
    data_b64: String,
    ext: String,
    collection_id: Option<i64>,
    nsfw: Option<i64>,
) -> Result<PasteResult, String> {
    use base64::Engine;
    let root = get_root(&state)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.as_bytes())
        .map_err(|e| format!("битые данные из буфера: {e}"))?;
    if bytes.is_empty() {
        return Err("буфер обмена пуст".into());
    }

    // Нормализуем расширение к тому, что понимает импортёр.
    let ext = match ext.trim().trim_start_matches('.').to_lowercase().as_str() {
        "jpeg" | "" => "jpg".to_string(),
        "x-png" | "apng" => "png".to_string(),
        other => other.to_string(),
    };

    // Пишем во временный файл, чтобы переиспользовать import_one (хэш, phash, превью).
    let mut tmp = std::env::temp_dir();
    tmp.push(format!("furrymanager_paste_{}.{ext}", unix_now()));
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;

    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    let lossless = load_config().lossless.unwrap_or(false);
    let res = import::import_one(conn, &root, &tmp, false, lossless, false);
    let _ = std::fs::remove_file(&tmp);
    let (file_id, is_new) = res?;

    // Свойства открытого вида: коллекция и метка SFW/NSFW.
    if let Some(cid) = collection_id {
        conn.execute(
            "INSERT OR IGNORE INTO collection_files (collection_id, file_id, added_at) \
             VALUES (?1, ?2, ?3)",
            params![cid, file_id, unix_now()],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(v) = nsfw {
        conn.execute(
            "UPDATE files SET nsfw = ?1 WHERE id = ?2",
            params![v, file_id],
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(PasteResult {
        imported: is_new,
        file_id,
    })
}

// ---------- Инструмент «Дубликаты» (near-dupe по phash) ----------

#[derive(Serialize)]
pub struct DupeFile {
    pub id: i64,
    pub path: String,
    pub thumb: Option<String>,
    pub ext: String,
    pub size: i64,
    pub width: i64,
    pub height: i64,
    pub kind: String,
}

#[derive(Serialize)]
pub struct DupeGroup {
    pub files: Vec<DupeFile>, // отсортированы: [0] — рекомендуемый к сохранению (крупнейший)
}

// Строка для кластеризации: (id, phash, ext, size, w, h, rel_path, thumb_rel, kind)
type DupeRow = (i64, i64, String, i64, i64, i64, String, Option<String>, String);

fn uf_find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]]; // сжатие пути
        x = parent[x];
    }
    x
}

/// Группирует почти-копии: ребро между файлами, если Хэмминг-расстояние phash ≤ thr.
fn cluster_dupes(rows: Vec<DupeRow>, root: &Path, thr: u32) -> Vec<DupeGroup> {
    let n = rows.len();
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for j in (i + 1)..n {
            let d = ((rows[i].1 as u64) ^ (rows[j].1 as u64)).count_ones();
            if d <= thr {
                let (a, b) = (uf_find(&mut parent, i), uf_find(&mut parent, j));
                if a != b {
                    parent[a] = b;
                }
            }
        }
    }
    let mut map: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for i in 0..n {
        let r = uf_find(&mut parent, i);
        map.entry(r).or_default().push(i);
    }
    let mut groups: Vec<DupeGroup> = Vec::new();
    for (_, idxs) in map {
        if idxs.len() < 2 {
            continue; // одиночки — не дубликаты
        }
        let mut files: Vec<DupeFile> = idxs
            .iter()
            .map(|&i| {
                let r = &rows[i];
                DupeFile {
                    id: r.0,
                    path: root.join(&r.6).to_string_lossy().into_owned(),
                    thumb: r.7.as_ref().map(|t| root.join(t).to_string_lossy().into_owned()),
                    ext: r.2.clone(),
                    size: r.3,
                    width: r.4,
                    height: r.5,
                    kind: r.8.clone(),
                }
            })
            .collect();
        // Рекомендуем оставить самый крупный по разрешению, затем по размеру файла.
        files.sort_by(|a, b| {
            (b.width * b.height, b.size).cmp(&(a.width * a.height, a.size))
        });
        groups.push(DupeGroup { files });
    }
    // Крупные группы — вперёд.
    groups.sort_by(|a, b| b.files.len().cmp(&a.files.len()));
    groups
}

/// Поиск почти-копий по perceptual hash. threshold — макс. расстояние Хэмминга (бит).
#[tauri::command]
pub async fn find_duplicates(
    state: State<'_, AppState>,
    threshold: Option<u32>,
) -> Result<Vec<DupeGroup>, String> {
    let root = get_root(&state)?;
    let thr = threshold.unwrap_or(8);
    // Читаем phash'и под замком, потом отпускаем — тяжёлый цикл без блокировки БД.
    let rows: Vec<DupeRow> = {
        let guard = state.conn.lock().unwrap();
        let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
        let mut stmt = conn
            .prepare(
                "SELECT id, phash, ext, size, width, height, rel_path, thumb_rel, kind \
                 FROM files WHERE phash IS NOT NULL",
            )
            .map_err(|e| e.to_string())?;
        // Привязываем к локальной переменной, чтобы временный MappedRows
        // освободил заём stmt/guard до конца блока (иначе E0597).
        let v = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, String>(8)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        v
    };

    let groups = tauri::async_runtime::spawn_blocking(move || cluster_dupes(rows, &root, thr))
        .await
        .map_err(|e| e.to_string())?;
    Ok(groups)
}

// ---------- Автораспределение по коллекциям ----------

#[derive(Serialize)]
pub struct AutoColGroup {
    pub collection_id: Option<i64>, // None — предлагается создать новую
    pub name: String,
    pub file_ids: Vec<i64>,
}

#[derive(Serialize)]
pub struct AutoColProposal {
    pub groups: Vec<AutoColGroup>,
    pub unmatched: Vec<i64>, // теги есть, но ни одна коллекция не подошла
    pub no_tags: Vec<i64>,   // тегов нет вовсе — нечем распределять
}

/// Предпросмотр: куда разложить file_ids по коллекциям на основе тегов.
/// Скоринг: характерные теги коллекции (частота среди её артов ≥ 0.4),
/// взвешенные IDF (редкий тег информативнее); порог совпадения 0.5.
/// create_new: группировать неподошедшие по общему редкому тегу (группа ≥ 3 —
/// защита от коллекций-одиночек).
#[tauri::command]
pub fn auto_collections_preview(
    state: State<AppState>,
    file_ids: Vec<i64>,
    create_new: bool,
) -> Result<AutoColProposal, String> {
    use std::collections::{HashMap, HashSet};
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;
    // Пустой список = вся библиотека (полное автораспределение после переиндексации).
    let file_ids: Vec<i64> = if file_ids.is_empty() {
        let mut s = conn
            .prepare("SELECT id FROM files")
            .map_err(|e| e.to_string())?;
        let v: Vec<i64> = s
            .query_map([], |r| r.get::<_, i64>(0))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        v
    } else {
        file_ids
    };
    if file_ids.is_empty() {
        return Err("Библиотека пуста".into());
    }

    let total_files: f64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())? as f64;

    // Частота тегов по библиотеке (df) и справочник тегов.
    let df: HashMap<i64, i64> = {
        let mut s = conn
            .prepare("SELECT tag_id, COUNT(*) FROM file_tags GROUP BY tag_id")
            .map_err(|e| e.to_string())?;
        let v: Vec<(i64, i64)> = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        v.into_iter().collect()
    };
    let tag_meta: HashMap<i64, (String, String)> = {
        let mut s = conn
            .prepare("SELECT id, namespace, name FROM tags")
            .map_err(|e| e.to_string())?;
        let v: Vec<(i64, String, String)> = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        v.into_iter().map(|(id, ns, nm)| (id, (ns, nm))).collect()
    };
    let idf = |t: i64| -> f64 {
        let d = *df.get(&t).unwrap_or(&1) as f64;
        (total_files / d.max(1.0)).ln().max(0.05)
    };
    // rating-теги не участвуют — это не «содержание» арта.
    let usable = |t: i64| tag_meta.get(&t).map(|(ns, _)| ns != "rating").unwrap_or(false);

    // Коллекции: размер и частоты тегов.
    let col_sizes: HashMap<i64, i64> = {
        let mut s = conn
            .prepare("SELECT collection_id, COUNT(*) FROM collection_files GROUP BY collection_id")
            .map_err(|e| e.to_string())?;
        let v: Vec<(i64, i64)> = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        v.into_iter().collect()
    };
    let col_names: HashMap<i64, String> = {
        let mut s = conn
            .prepare("SELECT id, name FROM collections")
            .map_err(|e| e.to_string())?;
        let v: Vec<(i64, String)> = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        v.into_iter().collect()
    };
    let mut col_tags: HashMap<i64, HashMap<i64, i64>> = HashMap::new();
    {
        let mut s = conn
            .prepare(
                "SELECT cf.collection_id, ft.tag_id, COUNT(*) FROM collection_files cf \
                 JOIN file_tags ft ON ft.file_id = cf.file_id \
                 GROUP BY cf.collection_id, ft.tag_id",
            )
            .map_err(|e| e.to_string())?;
        let v: Vec<(i64, i64, i64)> = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        for (cid, tid, cnt) in v {
            col_tags.entry(cid).or_default().insert(tid, cnt);
        }
    }

    // Характерные теги каждой коллекции: (tag, вес = частота * idf).
    let mut col_profile: HashMap<i64, Vec<(i64, f64)>> = HashMap::new();
    for (cid, tags) in &col_tags {
        let n_c = *col_sizes.get(cid).unwrap_or(&0) as f64;
        if n_c < 1.0 {
            continue;
        }
        let mut prof: Vec<(i64, f64)> = tags
            .iter()
            .filter(|(t, _)| usable(**t))
            .filter_map(|(t, cnt)| {
                let freq = *cnt as f64 / n_c;
                if freq >= 0.4 {
                    Some((*t, freq * idf(*t)))
                } else {
                    None
                }
            })
            .collect();
        prof.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        prof.truncate(20); // топ характерных — хвост слабых не размывает скоринг
        if !prof.is_empty() {
            col_profile.insert(*cid, prof);
        }
    }

    // Теги выбранных файлов и их текущее членство в коллекциях.
    let placeholders = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let file_tags: HashMap<i64, HashSet<i64>> = {
        let mut s = conn
            .prepare(&format!(
                "SELECT file_id, tag_id FROM file_tags WHERE file_id IN ({placeholders})"
            ))
            .map_err(|e| e.to_string())?;
        let v: Vec<(i64, i64)> = s
            .query_map(params_from_iter(file_ids.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        let mut m: HashMap<i64, HashSet<i64>> = HashMap::new();
        for (fid, tid) in v {
            if usable(tid) {
                m.entry(fid).or_default().insert(tid);
            }
        }
        m
    };
    let member_of: HashSet<(i64, i64)> = {
        let mut s = conn
            .prepare(&format!(
                "SELECT collection_id, file_id FROM collection_files WHERE file_id IN ({placeholders})"
            ))
            .map_err(|e| e.to_string())?;
        let v: Vec<(i64, i64)> = s
            .query_map(params_from_iter(file_ids.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        v.into_iter().collect()
    };

    // Скоринг: файл ложится в каждую коллекцию с score ≥ 0.5.
    const MATCH_THRESHOLD: f64 = 0.5;
    let mut groups_map: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut unmatched: Vec<i64> = Vec::new();
    let mut no_tags: Vec<i64> = Vec::new();

    for fid in &file_ids {
        let Some(tags) = file_tags.get(fid) else {
            no_tags.push(*fid);
            continue;
        };
        if tags.is_empty() {
            no_tags.push(*fid);
            continue;
        }
        let mut placed = false;
        for (cid, prof) in &col_profile {
            if member_of.contains(&(*cid, *fid)) {
                continue; // уже там
            }
            let total_w: f64 = prof.iter().map(|(_, w)| w).sum();
            let matched_w: f64 = prof
                .iter()
                .filter(|(t, _)| tags.contains(t))
                .map(|(_, w)| w)
                .sum();
            if total_w > 0.0 && matched_w / total_w >= MATCH_THRESHOLD {
                groups_map.entry(*cid).or_default().push(*fid);
                placed = true;
            }
        }
        if !placed {
            unmatched.push(*fid);
        }
    }

    let mut groups: Vec<AutoColGroup> = groups_map
        .into_iter()
        .map(|(cid, fids)| AutoColGroup {
            collection_id: Some(cid),
            name: col_names.get(&cid).cloned().unwrap_or_default(),
            file_ids: fids,
        })
        .collect();

    // Новые коллекции из «неподошедших»: общий редкий тег, группа минимум из 3 —
    // чтобы не плодить коллекции-одиночки.
    const MIN_NEW_GROUP: usize = 3;
    if create_new && unmatched.len() >= MIN_NEW_GROUP {
        // Кандидаты: не слишком общие теги (df ≤ 25% библиотеки).
        let mut tag_count: HashMap<i64, Vec<i64>> = HashMap::new();
        for fid in &unmatched {
            if let Some(tags) = file_tags.get(fid) {
                for t in tags {
                    let d = *df.get(t).unwrap_or(&0) as f64;
                    if d <= total_files * 0.25 {
                        tag_count.entry(*t).or_default().push(*fid);
                    }
                }
            }
        }
        let mut candidates: Vec<(i64, Vec<i64>)> = tag_count
            .into_iter()
            .filter(|(_, fids)| fids.len() >= MIN_NEW_GROUP)
            .collect();
        // Чем больше группа и реже тег — тем раньше берём.
        candidates.sort_by(|a, b| {
            b.1.len()
                .cmp(&a.1.len())
                .then_with(|| idf(b.0).partial_cmp(&idf(a.0)).unwrap_or(std::cmp::Ordering::Equal))
        });
        let mut taken: HashSet<i64> = HashSet::new();
        let mut still_unmatched: HashSet<i64> = unmatched.iter().copied().collect();
        for (tid, fids) in candidates {
            let fresh: Vec<i64> = fids.into_iter().filter(|f| !taken.contains(f)).collect();
            if fresh.len() < MIN_NEW_GROUP {
                continue;
            }
            let name = tag_meta
                .get(&tid)
                .map(|(_, nm)| nm.clone())
                .unwrap_or_else(|| "новая".into());
            for f in &fresh {
                taken.insert(*f);
                still_unmatched.remove(f);
            }
            groups.push(AutoColGroup {
                collection_id: None,
                name,
                file_ids: fresh,
            });
        }
        unmatched = still_unmatched.into_iter().collect();
    }

    groups.sort_by(|a, b| b.file_ids.len().cmp(&a.file_ids.len()));
    Ok(AutoColProposal {
        groups,
        unmatched,
        no_tags,
    })
}

/// Починка битых импортов: записи без превью/размеров классифицируются по
/// СОДЕРЖИМОМУ (iOS отдаёт HEIC/AVIF-байты как image/jpeg). HEIC/AVIF —
/// конвертируются, пустые/пропавшие — удаляются. Возвращает отчёт.
#[tauri::command]
pub fn repair_heic(state: State<AppState>) -> Result<serde_json::Value, String> {
    let root = get_root(&state)?;
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;

    let rows: Vec<(i64, String, Option<String>)> = {
        let mut s = conn
            .prepare(
                "SELECT id, rel_path, storage FROM files \
                 WHERE kind = 'image' AND (thumb_rel IS NULL OR width = 0)",
            )
            .map_err(|e| e.to_string())?;
        let v: Vec<(i64, String, Option<String>)> = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        v
    };

    let total = rows.len() as i64;
    let mut fixed = 0i64;
    let mut refreshed = 0i64;
    let mut deleted = 0i64;
    let mut classes: std::collections::HashMap<&'static str, i64> = std::collections::HashMap::new();

    for (id, rel, storage) in rows {
        let abs = root.join(&rel);
        let kind = import::sniff_kind(&abs);
        *classes.entry(kind).or_insert(0) += 1;
        match kind {
            "heic" | "avif" => {
                let conv = crate::heic::convert_one(&abs)?; // нет pillow-heif — ошибка наверх
                let res =
                    import::replace_with_converted(conn, &root, id, &abs, &conv, storage.as_deref());
                let _ = std::fs::remove_file(&conv);
                res?;
                fixed += 1;
            }
            // Знакомый формат: пробуем декодировать ещё раз. Целый — пересчитаем
            // превью/размеры; обрезанный (импорт оборвался) — спасать нечем, удаляем.
            "jpeg" | "png" | "gif" | "webp" => {
                if import::refresh_media_meta(conn, &root, id, &abs)? {
                    refreshed += 1;
                } else {
                    conn.execute("DELETE FROM files WHERE id = ?1", [id])
                        .map_err(|e| e.to_string())?;
                    if storage.as_deref() != Some("inplace") {
                        let _ = std::fs::remove_file(&abs);
                    }
                    deleted += 1;
                }
            }
            // Пустой или пропавший файл спасать нечем — убираем запись
            "empty" | "missing" => {
                conn.execute("DELETE FROM files WHERE id = ?1", [id])
                    .map_err(|e| e.to_string())?;
                if storage.as_deref() != Some("inplace") {
                    let _ = std::fs::remove_file(&abs);
                }
                deleted += 1;
            }
            _ => {}
        }
    }
    Ok(serde_json::json!({
        "candidates": total,
        "fixed": fixed,
        "refreshed": refreshed,
        "deleted": deleted,
        "classes": classes,
    }))
}

/// Установка ИИ из приложения: pip install + скачивание моделей.
/// Прогресс — событие "ai-setup" {stage, line}.
#[tauri::command]
pub async fn ai_setup(app: AppHandle, ai: State<'_, AiState>) -> Result<(), String> {
    let (script, python) = sidecar_cfg()?;
    let req = script
        .parent()
        .ok_or("нет папки sidecar")?
        .join("requirements.txt");

    // 1) pip install — стримим вывод в UI
    let app2 = app.clone();
    let py2 = python.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        use std::io::{BufRead, BufReader};
        use std::process::Stdio;
        let mut cmd = std::process::Command::new(&py2);
        cmd.args(["-m", "pip", "install", "-r"])
            .arg(&req)
            .args(["--disable-pip-version-check", "--no-input"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Не удалось запустить pip: {e}. Установлен ли Python?"))?;

        // stderr читаем в отдельном потоке, чтобы не заблокироваться
        let app_err = app2.clone();
        let stderr = child.stderr.take();
        let err_thread = std::thread::spawn(move || {
            if let Some(se) = stderr {
                for line in BufReader::new(se).lines().flatten() {
                    let _ = app_err.emit("ai-setup", serde_json::json!({"stage":"pip","line": line}));
                }
            }
        });
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines().flatten() {
                let _ = app2.emit("ai-setup", serde_json::json!({"stage":"pip","line": line}));
            }
        }
        let _ = err_thread.join();
        let status = child.wait().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("pip завершился с ошибкой — детали выше в прогрессе".into());
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())??;

    // 2) Скачивание моделей (warmup сайдкара)
    let _ = app.emit(
        "ai-setup",
        serde_json::json!({"stage":"models","line":"Скачиваю нейросети (~1 ГБ, один раз)… Это может занять 5–20 минут."}),
    );
    let sc = ai.sidecar.clone();
    let (s2, p2) = (script.clone(), python.clone());
    tauri::async_runtime::spawn_blocking(move || {
        sc.rpc(&s2, &p2, "warmup", serde_json::json!({}))
    })
    .await
    .map_err(|e| e.to_string())??;

    let _ = app.emit("ai-setup", serde_json::json!({"stage":"done","line":"Готово!"}));
    Ok(())
}

// ---------- Статистика ----------

#[derive(Serialize)]
pub struct Stats {
    pub files: i64,
    pub images: i64,
    pub videos: i64,
    pub total_bytes: i64,
    pub dupes_skipped: i64,
}

#[tauri::command]
pub fn library_stats(state: State<AppState>) -> Result<Stats, String> {
    let guard = state.conn.lock().unwrap();
    let conn = guard.as_ref().ok_or("Библиотека не настроена")?;

    let (files, total_bytes): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM files",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    let images: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE kind IN ('image','animation')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let videos: i64 = conn
        .query_row("SELECT COUNT(*) FROM files WHERE kind = 'video'", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    let dupes_skipped: i64 = conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'dupes_skipped'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or(0);

    Ok(Stats {
        files,
        images,
        videos,
        total_bytes,
        dupes_skipped,
    })
}
