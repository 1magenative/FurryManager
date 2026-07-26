//! Telegram-бот-загрузчик: всё, что кинули боту (фото/видео/гиф, в т.ч.
//! пересланное из каналов), скачивается в библиотеку. Текст игнорируется.
//! Опрос getUpdates (long polling) при запуске и далее в цикле.
//!
//! Ограничение Bot API: бот скачивает файлы только до ~20 МБ. Большие видео
//! Telegram не отдаёт боту — такие пропускаются с сообщением об ошибке.

use crate::commands::{load_config, save_config};
use crate::{db, import};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

static POLLING: AtomicBool = AtomicBool::new(false);

fn client() -> Option<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(65)) // long polling держит соединение до 30с
        .build()
        .ok()
}

/// Запустить опрос, если бот включён и есть токен (вызывается при старте app).
pub fn start_if_enabled(app: AppHandle) {
    let cfg = load_config();
    let has_token = cfg.tg_token.as_deref().map(|t| !t.is_empty()).unwrap_or(false);
    if cfg.tg_bot.unwrap_or(false) && has_token {
        start(app);
    }
}

/// Запустить поток опроса (не более одного одновременно).
pub fn start(app: AppHandle) {
    if POLLING.swap(true, Ordering::SeqCst) {
        return; // уже работает
    }
    std::thread::spawn(move || {
        poll_loop(app);
        POLLING.store(false, Ordering::SeqCst);
    });
}

fn poll_loop(app: AppHandle) {
    let Some(cl) = client() else { return };
    loop {
        let cfg = load_config();
        if !cfg.tg_bot.unwrap_or(false) {
            break; // выключили в настройках
        }
        let token = match cfg.tg_token {
            Some(t) if !t.is_empty() => t,
            _ => break,
        };
        let offset = cfg.tg_offset.unwrap_or(0);
        match get_updates(&cl, &token, offset) {
            Ok(updates) => {
                let mut next = offset;
                for u in &updates {
                    let uid = u.get("update_id").and_then(|v| v.as_i64()).unwrap_or(0);
                    if uid + 1 > next {
                        next = uid + 1;
                    }
                    handle_update(&app, &cl, &token, u);
                }
                if next != offset {
                    let mut c = load_config();
                    c.tg_offset = Some(next);
                    let _ = save_config(&c);
                }
            }
            Err(_) => std::thread::sleep(Duration::from_secs(5)),
        }
    }
}

fn get_updates(cl: &reqwest::blocking::Client, token: &str, offset: i64) -> Result<Vec<Value>, String> {
    // По умолчанию getUpdates отдаёт message и channel_post — нам этого хватает.
    let url = format!("https://api.telegram.org/bot{token}/getUpdates?timeout=30&offset={offset}");
    let resp: Value = cl
        .get(&url)
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return Err("telegram: ok=false".into());
    }
    Ok(resp
        .get("result")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

fn handle_update(app: &AppHandle, cl: &reqwest::blocking::Client, token: &str, u: &Value) {
    // Пересланное сообщение приходит теми же полями media — отдельной обработки не нужно.
    let Some(msg) = u.get("message").or_else(|| u.get("channel_post")) else { return };
    let Some((file_id, hint_ext)) = pick_media(msg) else { return }; // текст/прочее — игнор
    let _ = app.emit("web-import", json!({"stage":"start","line":"Telegram: получаю файл…"}));
    match download_and_import(cl, token, &file_id, &hint_ext) {
        Ok(true) => {
            let _ = app.emit("web-import", json!({"stage":"done","line":"Из Telegram добавлено ✓"}));
        }
        Ok(false) => {
            let _ = app.emit("web-import", json!({"stage":"done","line":"Из Telegram: уже было (дубликат)"}));
        }
        Err(e) => {
            let _ = app.emit("web-import", json!({"stage":"error","line":format!("Telegram: {e}")}));
        }
    }
}

/// file_id самого крупного медиа сообщения (фото/видео/гиф/документ-картинка).
fn pick_media(msg: &Value) -> Option<(String, String)> {
    if let Some(photos) = msg.get("photo").and_then(|v| v.as_array()) {
        if let Some(id) = photos.last().and_then(|p| p.get("file_id")).and_then(|v| v.as_str()) {
            return Some((id.to_string(), "jpg".into())); // последний размер — самый крупный
        }
    }
    if let Some(a) = msg.get("animation") {
        if let Some(id) = a.get("file_id").and_then(|v| v.as_str()) {
            let ext = a
                .get("file_name")
                .and_then(|v| v.as_str())
                .and_then(ext_of)
                .unwrap_or_else(|| "mp4".into());
            return Some((id.to_string(), ext));
        }
    }
    if let Some(id) = msg.get("video").and_then(|v| v.get("file_id")).and_then(|v| v.as_str()) {
        return Some((id.to_string(), "mp4".into()));
    }
    if let Some(d) = msg.get("document") {
        let mime = d.get("mime_type").and_then(|v| v.as_str()).unwrap_or("");
        if mime.starts_with("image/") || mime.starts_with("video/") {
            if let Some(id) = d.get("file_id").and_then(|v| v.as_str()) {
                let ext = d
                    .get("file_name")
                    .and_then(|v| v.as_str())
                    .and_then(ext_of)
                    .or_else(|| mime_ext(mime))
                    .unwrap_or_else(|| "bin".into());
                return Some((id.to_string(), ext));
            }
        }
    }
    None
}

fn ext_of(name: &str) -> Option<String> {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
}

fn mime_ext(mime: &str) -> Option<String> {
    Some(
        match mime {
            "image/jpeg" => "jpg",
            "image/png" => "png",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "video/mp4" => "mp4",
            "video/webm" => "webm",
            "video/quicktime" => "mov",
            _ => return None,
        }
        .into(),
    )
}

fn download_and_import(
    cl: &reqwest::blocking::Client,
    token: &str,
    file_id: &str,
    hint_ext: &str,
) -> Result<bool, String> {
    // getFile → file_path
    let url = format!("https://api.telegram.org/bot{token}/getFile?file_id={file_id}");
    let resp: Value = cl
        .get(&url)
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return Err("файл недоступен (возможно, больше 20 МБ — ограничение Telegram)".into());
    }
    let file_path = resp
        .get("result")
        .and_then(|r| r.get("file_path"))
        .and_then(|v| v.as_str())
        .ok_or("нет file_path")?;
    let ext = ext_of(file_path).unwrap_or_else(|| hint_ext.to_string());

    let file_url = format!("https://api.telegram.org/file/bot{token}/{file_path}");
    let bytes = cl
        .get(&file_url)
        .send()
        .map_err(|e| e.to_string())?
        .bytes()
        .map_err(|e| e.to_string())?;
    if bytes.is_empty() {
        return Err("пустой файл".into());
    }

    let cfg = load_config();
    let root = PathBuf::from(cfg.library_root.ok_or("Библиотека не настроена")?);
    let lossless = cfg.lossless.unwrap_or(false);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut tmp = std::env::temp_dir();
    tmp.push(format!("fm_tg_{ts}.{ext}"));
    std::fs::write(&tmp, bytes.as_ref()).map_err(|e| e.to_string())?;
    let conn = db::open(&root.join("library.db")).map_err(|e| e.to_string())?;
    let res = import::import_one(&conn, &root, &tmp, false, lossless, false).map(|(_, is_new)| is_new);
    let _ = std::fs::remove_file(&tmp);
    res
}

// ---------- Команды для настроек ----------

/// Проверить токен через getMe, включить бота и запустить опрос.
#[tauri::command]
pub async fn tg_enable(app: AppHandle, token: String) -> Result<Value, String> {
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err("Пустой токен".into());
    }
    let t2 = token.clone();
    let username = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let cl = client().ok_or("нет http-клиента")?;
        let url = format!("https://api.telegram.org/bot{t2}/getMe");
        let resp: Value = cl
            .get(&url)
            .send()
            .map_err(|e| e.to_string())?
            .json()
            .map_err(|e| e.to_string())?;
        if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            return Err("Токен не подошёл. Проверь его у @BotFather.".into());
        }
        Ok(resp
            .get("result")
            .and_then(|r| r.get("username"))
            .and_then(|v| v.as_str())
            .unwrap_or("bot")
            .to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    let mut c = load_config();
    c.tg_token = Some(token);
    c.tg_bot = Some(true);
    save_config(&c)?;
    start(app);
    Ok(json!({ "username": username }))
}

/// Выключить бота (опрос сам остановится на следующем цикле).
#[tauri::command]
pub fn tg_disable() -> Result<(), String> {
    let mut c = load_config();
    c.tg_bot = Some(false);
    save_config(&c)
}

/// Состояние для настроек (токен наружу не отдаём).
#[tauri::command]
pub fn tg_status() -> Value {
    let c = load_config();
    json!({
        "enabled": c.tg_bot.unwrap_or(false),
        "has_token": c.tg_token.as_deref().map(|t| !t.is_empty()).unwrap_or(false),
    })
}
