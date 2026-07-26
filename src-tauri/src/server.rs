//! Локальный HTTP-сервер для браузерного расширения.
//! POST /ingest {token, url} → скачивание через gallery-dl (сайдкар) → импорт.

use crate::commands::{load_config, sidecar_cfg_pub, AiState};
use crate::{db, import};
use rusqlite::{params, params_from_iter};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};

/// Мобильная страница (PWA) и её иконка — вшиты в бинарник.
const PWA_HTML: &str = include_str!("../pwa/index.html");
const PWA_ICON: &[u8] = include_bytes!("../icons/pwa-icon.png"); // непрозрачная: iOS красит альфу в чёрный
const PWA_SW: &str = include_str!("../pwa/sw.js"); // Service Worker: офлайн-запуск (нужен https через Tailscale)

pub fn start(app: AppHandle, port: u16, token: String, lan: bool) {
    std::thread::spawn(move || {
        // LAN-режим (доступ с телефона) — только по явной галочке в настройках.
        let host = if lan { "0.0.0.0" } else { "127.0.0.1" };
        let server = match tiny_http::Server::http((host, port)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[server] не удалось открыть порт {port}: {e}");
                return;
            }
        };
        eprintln!("[server] слушаю {host}:{port} (расширение{})",
                  if lan { " + телефон" } else { "" });
        for mut request in server.incoming_requests() {
            // ВАЖНО: тело читаем не сразу — большие файлы (видео с телефона)
            // стримятся на диск, иначе ролик целиком лёг бы в память.

            // CORS/preflight — чтобы расширению было проще
            let cors = |resp: tiny_http::Response<std::io::Cursor<Vec<u8>>>| {
                resp.with_header(
                    tiny_http::Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap(),
                )
                .with_header(
                    tiny_http::Header::from_bytes(
                        "Access-Control-Allow-Headers",
                        "content-type",
                    )
                    .unwrap(),
                )
                .with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                )
            };
            let reply = |request: tiny_http::Request, code: u16, s: &str| {
                let _ = request.respond(
                    cors(tiny_http::Response::from_string(s.to_string())).with_status_code(code),
                );
            };

            if request.method() == &tiny_http::Method::Options {
                reply(request, 200, "{}");
                continue;
            }
            let (path, qs) = split_query(request.url());
            // Проверка доступности (для настройки телефона)
            if path == "/ping" {
                reply(request, 200, r#"{"ok":true,"app":"furrymanager"}"#);
                continue;
            }
            let authed = qs.get("token").map(|t| t == &token).unwrap_or(false);

            if request.method() == &tiny_http::Method::Get {
                match path.as_str() {
                    "/" | "/index.html" => {
                        // no-store: страница обновляется при каждом заходе, иначе
                        // Safari/standalone-PWA держит старую версию после пересборки.
                        let resp = tiny_http::Response::from_string(PWA_HTML)
                            .with_header(
                                tiny_http::Header::from_bytes(
                                    "Content-Type",
                                    "text/html; charset=utf-8",
                                )
                                .unwrap(),
                            )
                            .with_header(
                                tiny_http::Header::from_bytes(
                                    "Cache-Control",
                                    "no-store, must-revalidate",
                                )
                                .unwrap(),
                            );
                        let _ = request.respond(resp);
                        continue;
                    }
                    "/icon.png" => {
                        let resp = tiny_http::Response::from_data(PWA_ICON.to_vec()).with_header(
                            tiny_http::Header::from_bytes("Content-Type", "image/png").unwrap(),
                        );
                        let _ = request.respond(resp);
                        continue;
                    }
                    "/sw.js" => {
                        // Service Worker: no-cache, чтобы обновление подхватывалось.
                        // Токен не нужен — скрипт не выдаёт данные.
                        let resp = tiny_http::Response::from_string(PWA_SW)
                            .with_header(
                                tiny_http::Header::from_bytes(
                                    "Content-Type",
                                    "application/javascript; charset=utf-8",
                                )
                                .unwrap(),
                            )
                            .with_header(
                                tiny_http::Header::from_bytes("Cache-Control", "no-cache").unwrap(),
                            );
                        let _ = request.respond(resp);
                        continue;
                    }
                    "/manifest.json" => {
                        // Токен обязателен: в манифест вшивается start_url с ним.
                        if !authed {
                            reply(request, 403, r#"{"error":"bad token"}"#);
                            continue;
                        }
                        let m = json!({
                            "name": "FurryManager",
                            "short_name": "FurryM",
                            "start_url": format!("/?token={token}"),
                            "display": "standalone",
                            "background_color": "#16161c",
                            "theme_color": "#16161c",
                            "icons": [{"src": "/icon.png", "sizes": "180x180", "type": "image/png"}]
                        });
                        reply(request, 200, &m.to_string());
                        continue;
                    }
                    _ => {}
                }
                if path == "/api/files"
                    || path == "/api/collections"
                    || path == "/api/config"
                    || path == "/api/group"
                    || path.starts_with("/thumb/")
                    || path.starts_with("/media/")
                {
                    if !authed {
                        reply(request, 403, r#"{"error":"bad token"}"#);
                        continue;
                    }
                    handle_get_api(request, &path, &qs);
                    continue;
                }
            }

            // Потоковая загрузка файла: тело пишем прямо на диск, без base64 и
            // без буфера в памяти — так проходят большие видео с телефона.
            if path == "/ingest-raw" {
                if !authed {
                    reply(request, 403, r#"{"error":"bad token"}"#);
                    continue;
                }
                let ext = normalize_ext(qs.get("ext").map(|s| s.as_str()).unwrap_or(""));
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let mut tmp = std::env::temp_dir();
                tmp.push(format!("fm_up_{ts}.{ext}"));
                let saved = match std::fs::File::create(&tmp) {
                    Ok(mut out) => std::io::copy(request.as_reader(), &mut out),
                    Err(e) => Err(e),
                };
                match saved {
                    Ok(n) if n > 0 => {
                        reply(request, 200, r#"{"ok":true}"#);
                        let app2 = app.clone();
                        std::thread::spawn(move || process_temp_file(app2, tmp));
                    }
                    Ok(_) => {
                        let _ = std::fs::remove_file(&tmp);
                        reply(request, 400, r#"{"error":"empty body"}"#);
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&tmp);
                        reply(request, 500, &format!(r#"{{"error":"{e}"}}"#));
                    }
                }
                continue;
            }

            if path != "/ingest" && path != "/ingest-file" {
                reply(request, 404, r#"{"error":"not found"}"#);
                continue;
            }
            // JSON-эндпоинты: тело маленькое, читаем строкой
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            let val: serde_json::Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(_) => {
                    reply(request, 400, r#"{"error":"bad json"}"#);
                    continue;
                }
            };
            if val.get("token").and_then(|t| t.as_str()) != Some(token.as_str()) {
                reply(request, 403, r#"{"error":"bad token"}"#);
                continue;
            }
            if path == "/ingest" {
                let Some(url) = val.get("url").and_then(|u| u.as_str()).map(String::from) else {
                    reply(request, 400, r#"{"error":"no url"}"#);
                    continue;
                };
                // Отвечаем сразу, качаем в фоне
                reply(request, 200, r#"{"ok":true}"#);
                let app2 = app.clone();
                std::thread::spawn(move || process_url(app2, url));
            } else {
                // /ingest-file: {token, data_b64, ext} — файл с телефона (или другого клиента)
                let Some(data) = val.get("data_b64").and_then(|d| d.as_str()).map(String::from)
                else {
                    reply(request, 400, r#"{"error":"no data_b64"}"#);
                    continue;
                };
                let ext = val
                    .get("ext")
                    .and_then(|e| e.as_str())
                    .unwrap_or("png")
                    .to_string();
                reply(request, 200, r#"{"ok":true}"#);
                let app2 = app.clone();
                std::thread::spawn(move || process_file(app2, data, ext));
            }
        }
    });
}

/// Нормализация расширения от телефона (MIME-подтип или суффикс имени).
fn normalize_ext(raw: &str) -> String {
    let e = raw.trim().trim_start_matches('.').to_lowercase();
    match e.as_str() {
        "jpeg" | "" => "jpg".to_string(),
        "x-png" | "apng" => "png".to_string(),
        "quicktime" => "mov".to_string(),
        "x-m4v" => "m4v".to_string(),
        "x-matroska" => "mkv".to_string(),
        other => other.to_string(),
    }
}

/// Импорт уже сохранённого временного файла (потоковая загрузка).
fn process_temp_file(app: AppHandle, tmp: PathBuf) {
    let _ = app.emit("web-import", json!({"stage": "start", "line": "Получаю файл…"}));
    let res = import_temp(&tmp);
    let _ = std::fs::remove_file(&tmp);
    match res {
        Ok(true) => {
            let _ = app.emit("web-import", json!({"stage": "done", "line": "Добавлено ✓"}));
        }
        Ok(false) => {
            let _ = app.emit(
                "web-import",
                json!({"stage": "done", "line": "Уже было в библиотеке (дубликат)"}),
            );
        }
        Err(e) => {
            let _ = app.emit("web-import", json!({"stage": "error", "line": e}));
        }
    }
}

/// Общий импорт временного файла в библиотеку.
fn import_temp(tmp: &Path) -> Result<bool, String> {
    let cfg = load_config();
    let root = PathBuf::from(cfg.library_root.ok_or("Библиотека не настроена")?);
    let conn = db::open(&root.join("library.db")).map_err(|e| e.to_string())?;
    let lossless = cfg.lossless.unwrap_or(false);
    let (_id, is_new) = import::import_one(&conn, &root, tmp, false, lossless, false)?;
    Ok(is_new)
}

/// Импорт файла, присланного телефоном (base64) — в фоне, с тостом в UI.
fn process_file(app: AppHandle, data_b64: String, ext: String) {
    let _ = app.emit("web-import", json!({"stage": "start", "line": "Получаю файл…"}));
    match ingest_file(&data_b64, &ext) {
        Ok(true) => {
            let _ = app.emit("web-import", json!({"stage": "done", "line": "Добавлено ✓"}));
        }
        Ok(false) => {
            let _ = app.emit(
                "web-import",
                json!({"stage": "done", "line": "Уже было в библиотеке (дубликат)"}),
            );
        }
        Err(e) => {
            let _ = app.emit("web-import", json!({"stage": "error", "line": e}));
        }
    }
}

// ---------- GET-API для мобильной страницы ----------

fn json_resp(request: tiny_http::Request, code: u16, s: String) {
    let resp = tiny_http::Response::from_string(s)
        .with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap())
        .with_header(tiny_http::Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap())
        .with_status_code(code);
    let _ = request.respond(resp);
}

fn handle_get_api(request: tiny_http::Request, path: &str, qs: &HashMap<String, String>) {
    let cfg = load_config();
    let Some(root) = cfg.library_root.map(PathBuf::from) else {
        json_resp(request, 500, r#"{"error":"библиотека не настроена"}"#.into());
        return;
    };
    let conn = match db::open(&root.join("library.db")) {
        Ok(c) => c,
        Err(e) => {
            json_resp(request, 500, format!(r#"{{"error":"{e}"}}"#));
            return;
        }
    };

    if path == "/api/config" {
        // Настройки, которые нужны мобильной странице (фильтр при запуске)
        json_resp(
            request,
            200,
            json!({
                "nsfw_split": cfg.nsfw_split.unwrap_or(false),
                "default_nsfw": cfg.default_nsfw,
            })
            .to_string(),
        );
        return;
    }

    // Страницы группы (комикс/вариации) для мобильного просмотра
    if path == "/api/group" {
        let Some(id) = qs.get("id").and_then(|v| v.parse::<i64>().ok()) else {
            return json_resp(request, 400, r#"{"error":"bad id"}"#.into());
        };
        let (name, kind): (String, String) = match conn.query_row(
            "SELECT name, kind FROM groups WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            Ok(v) => v,
            Err(_) => return json_resp(request, 404, r#"{"error":"группа не найдена"}"#.into()),
        };
        let mut stmt = match conn.prepare(
            "SELECT f.id, f.kind, f.ext, f.width, f.height, (f.thumb_rel IS NOT NULL) \
             FROM files f WHERE f.group_id = ?1 ORDER BY COALESCE(f.group_pos, 0), f.id",
        ) {
            Ok(s) => s,
            Err(e) => return json_resp(request, 500, format!(r#"{{"error":"{e}"}}"#)),
        };
        let files: Vec<serde_json::Value> = stmt
            .query_map([id], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "ext": r.get::<_, String>(2)?,
                    "width": r.get::<_, i64>(3)?,
                    "height": r.get::<_, i64>(4)?,
                    "has_thumb": r.get::<_, bool>(5)?,
                    "group_count": 0
                }))
            })
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        json_resp(
            request,
            200,
            json!({ "id": id, "name": name, "kind": kind, "files": files }).to_string(),
        );
        return;
    }

    if path == "/api/collections" {
        let mut stmt = match conn.prepare(
            "SELECT c.id, c.name, COUNT(cf.file_id) FROM collections c \
             LEFT JOIN collection_files cf ON cf.collection_id = c.id \
             GROUP BY c.id ORDER BY COALESCE(c.position, 999999), c.name",
        ) {
            Ok(s) => s,
            Err(e) => return json_resp(request, 500, format!(r#"{{"error":"{e}"}}"#)),
        };
        let rows: Vec<serde_json::Value> = stmt
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "count": r.get::<_, i64>(2)?
                }))
            })
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        json_resp(request, 200, serde_json::Value::Array(rows).to_string());
        return;
    }

    if path == "/api/files" {
        let offset: i64 = qs.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
        let limit: i64 = qs
            .get("limit")
            .and_then(|v| v.parse().ok())
            .unwrap_or(60)
            .clamp(1, 200);
        let search = qs.get("search").cloned().unwrap_or_default();

        let mut sql = String::from(
            "SELECT f.id, f.kind, f.ext, f.width, f.height, \
             (f.thumb_rel IS NOT NULL), \
             COALESCE((SELECT COUNT(*) FROM files fg WHERE fg.group_id = f.group_id), 0), \
             f.group_id \
             FROM files f WHERE 1=1",
        );
        // Группы и в PWA выглядят одной стопкой
        sql.push_str(crate::commands::GROUP_COVER_WHERE);
        if let Some(c) = qs.get("collection").and_then(|v| v.parse::<i64>().ok()) {
            if c == -1 {
                sql.push_str(
                    " AND NOT EXISTS (SELECT 1 FROM collection_files cf WHERE cf.file_id = f.id)",
                );
            } else {
                sql.push_str(&format!(
                    " AND EXISTS (SELECT 1 FROM collection_files cf \
                     WHERE cf.file_id = f.id AND cf.collection_id = {c})"
                ));
            }
        }
        if let Some(n) = qs.get("nsfw").and_then(|v| v.parse::<i64>().ok()) {
            sql.push_str(&crate::commands::nsfw_clause(n));
        }
        let filters = crate::commands::build_filters(&search);
        sql.push_str(&filters.sql);
        let mut binds = filters.binds;
        for w in &filters.free {
            sql.push_str(
                " AND (EXISTS (SELECT 1 FROM file_tags ft JOIN tags tg ON tg.id = ft.tag_id \
                 WHERE ft.file_id = f.id AND tg.name LIKE ?) \
                 OR EXISTS (SELECT 1 FROM sources s WHERE s.file_id = f.id \
                 AND (s.artist LIKE ? OR s.title LIKE ?)))",
            );
            let pat = format!("%{w}%");
            binds.push(pat.clone());
            binds.push(pat.clone());
            binds.push(pat);
        }
        sql.push_str(&format!(
            " ORDER BY f.imported_at DESC, f.id DESC LIMIT {limit} OFFSET {}",
            offset.max(0)
        ));

        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => return json_resp(request, 500, format!(r#"{{"error":"{e}"}}"#)),
        };
        let rows: Vec<serde_json::Value> = stmt
            .query_map(params_from_iter(binds.iter()), |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "ext": r.get::<_, String>(2)?,
                    "width": r.get::<_, i64>(3)?,
                    "height": r.get::<_, i64>(4)?,
                    "has_thumb": r.get::<_, bool>(5)?,
                    "group_count": r.get::<_, i64>(6)?,
                    "group_id": r.get::<_, Option<i64>>(7)?
                }))
            })
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        json_resp(request, 200, serde_json::Value::Array(rows).to_string());
        return;
    }

    // /thumb/{id} и /media/{id}
    if let Some(idstr) = path.strip_prefix("/thumb/") {
        let Ok(id) = idstr.parse::<i64>() else {
            return json_resp(request, 400, r#"{"error":"bad id"}"#.into());
        };
        let row: Result<(Option<String>, String, String), _> = conn.query_row(
            "SELECT thumb_rel, rel_path, ext FROM files WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        );
        match row {
            Ok((Some(t), _, _)) => serve_media(request, &root.join(t), "jpg"),
            // Нет превью — отдаём оригинал (мелкие картинки)
            Ok((None, rel, ext)) => serve_media(request, &root.join(rel), &ext),
            Err(_) => json_resp(request, 404, r#"{"error":"not found"}"#.into()),
        }
        return;
    }
    if let Some(idstr) = path.strip_prefix("/media/") {
        let Ok(id) = idstr.parse::<i64>() else {
            return json_resp(request, 400, r#"{"error":"bad id"}"#.into());
        };
        let row: Result<(String, String), _> = conn.query_row(
            "SELECT rel_path, ext FROM files WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        );
        match row {
            // Для in-place в rel_path абсолютный путь; root.join(abs) вернёт abs как есть.
            Ok((rel, ext)) => serve_media(request, &root.join(rel), &ext),
            Err(_) => json_resp(request, 404, r#"{"error":"not found"}"#.into()),
        }
        return;
    }
    json_resp(request, 404, r#"{"error":"not found"}"#.into());
}

fn content_type(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" | "jfif" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

/// Отдаёт файл; поддерживает Range (iOS Safari не играет видео без 206).
fn serve_media(request: tiny_http::Request, abs: &Path, ext: &str) {
    let Ok(mut f) = std::fs::File::open(abs) else {
        return json_resp(request, 404, r#"{"error":"file missing"}"#.into());
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let ct = tiny_http::Header::from_bytes("Content-Type", content_type(ext)).unwrap();
    let ar = tiny_http::Header::from_bytes("Accept-Ranges", "bytes").unwrap();
    // Файлы адресуются по хешу — можно кэшировать надолго. Без этого iOS Safari
    // перезапрашивает превью при быстром скролле → мельтешение в ленте.
    let cc = tiny_http::Header::from_bytes("Cache-Control", "private, max-age=604800").unwrap();

    let range = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Range"))
        .map(|h| h.value.as_str().to_string());

    if let Some((start, end)) = range.as_deref().and_then(|h| parse_range(h, len)) {
        use std::io::{Read, Seek, SeekFrom};
        let _ = f.seek(SeekFrom::Start(start));
        let n = end - start + 1;
        let cr = tiny_http::Header::from_bytes(
            "Content-Range",
            format!("bytes {start}-{end}/{len}"),
        )
        .unwrap();
        let resp = tiny_http::Response::new(
            tiny_http::StatusCode(206),
            vec![ct, ar, cc, cr],
            f.take(n),
            Some(n as usize),
            None,
        );
        let _ = request.respond(resp);
    } else {
        let resp = tiny_http::Response::from_file(f)
            .with_header(ct)
            .with_header(ar)
            .with_header(cc);
        let _ = request.respond(resp);
    }
}

/// "bytes=a-b" | "bytes=a-" | "bytes=-suffix" → (start, end) включительно.
fn parse_range(h: &str, len: u64) -> Option<(u64, u64)> {
    if len == 0 {
        return None;
    }
    let r = h.trim().strip_prefix("bytes=")?;
    let (a, b) = r.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() {
        let suffix: u64 = b.parse().ok()?;
        Some((len.saturating_sub(suffix), len - 1))
    } else {
        let start: u64 = a.parse().ok()?;
        let end: u64 = if b.is_empty() { len - 1 } else { b.parse().ok()? };
        if start > end || end >= len {
            None
        } else {
            Some((start, end))
        }
    }
}

/// Разбирает URL на путь и query-параметры (с процентным декодированием значений).
fn split_query(url: &str) -> (String, HashMap<String, String>) {
    let mut map = HashMap::new();
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    };
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(urldecode(k), urldecode(v));
    }
    (path.to_string(), map)
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                // %XX
                if let Ok(b) = u8::from_str_radix(
                    std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                    16,
                ) {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn ingest_file(data_b64: &str, ext: &str) -> Result<bool, String> {
    use base64::Engine;
    // iOS Shortcuts может вставлять переводы строк в base64 — вычищаем.
    let clean: String = data_b64.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(clean.as_bytes())
        .map_err(|e| format!("битый base64: {e}"))?;
    if bytes.is_empty() {
        return Err("пустые данные".into());
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut tmp = std::env::temp_dir();
    tmp.push(format!("fm_ingest_{ts}.{}", normalize_ext(ext)));
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    let res = import_temp(&tmp);
    let _ = std::fs::remove_file(&tmp);
    res
}

fn process_url(app: AppHandle, url: String) {
    let _ = app.emit("web-import", json!({"stage": "start", "line": format!("Скачиваю: {url}")}));
    let result = download_and_import(&app, &url);
    match result {
        Ok((imported, dupes)) => {
            let _ = app.emit(
                "web-import",
                json!({"stage": "done",
                       "line": format!("Готово: добавлено {imported}, дубликатов {dupes}")}),
            );
        }
        Err(e) => {
            let _ = app.emit("web-import", json!({"stage": "error", "line": e}));
        }
    }
}

fn download_and_import(app: &AppHandle, url: &str) -> Result<(i64, i64), String> {
    let cfg = load_config();
    let root = PathBuf::from(cfg.library_root.ok_or("Библиотека не настроена")?);
    let (script, python) = sidecar_cfg_pub()?;

    // Сайдкар качает через gallery-dl и возвращает файлы + метаданные
    let ai = app.state::<AiState>();
    let res = ai.sidecar.rpc(
        &script,
        &python,
        "download",
        json!({ "url": url, "cookies": cfg.browser_cookies.clone().unwrap_or_default() }),
    )?;

    let items = res
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or("gallery-dl ничего не скачал")?;
    let tmp_dir = res.get("dir").and_then(|v| v.as_str()).map(String::from);

    let conn = db::open(&root.join("library.db")).map_err(|e| e.to_string())?;
    let lossless = cfg.lossless.unwrap_or(false);
    let (mut imported, mut dupes) = (0i64, 0i64);

    for it in items {
        let Some(path) = it.get("path").and_then(|p| p.as_str()) else { continue };
        let (file_id, is_new) = match import::import_one(&conn, &root, std::path::Path::new(path), false, lossless, false) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[server] импорт {path}: {e}");
                continue;
            }
        };
        if is_new { imported += 1 } else { dupes += 1 }

        // Источник (даже для дубликата — знание, где ещё лежит арт, полезно)
        let (src_url, site, artist, title) = (
            it.get("url").and_then(|v| v.as_str()).unwrap_or(url),
            it.get("site").and_then(|v| v.as_str()).unwrap_or(""),
            it.get("artist").and_then(|v| v.as_str()),
            it.get("title").and_then(|v| v.as_str()),
        );
        let _ = conn.execute(
            "INSERT INTO sources (file_id, url, site, artist, title) \
             SELECT ?1, ?2, ?3, ?4, ?5 WHERE NOT EXISTS \
             (SELECT 1 FROM sources WHERE file_id = ?1 AND url = ?2)",
            params![file_id, src_url, site, artist, title],
        );

        // Теги сайта (namespace из e621-категорий, origin = source-site)
        if let Some(tags) = it.get("tags").and_then(|v| v.as_array()) {
            for t in tags {
                let ns = t.get(0).and_then(|v| v.as_str()).unwrap_or("");
                let name = t.get(1).and_then(|v| v.as_str()).unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO tags (namespace, name) VALUES (?1, ?2)",
                    params![ns, name.to_lowercase()],
                );
                if let Ok(tag_id) = conn.query_row(
                    "SELECT id FROM tags WHERE namespace = ?1 AND name = ?2",
                    params![ns, name.to_lowercase()],
                    |r| r.get::<_, i64>(0),
                ) {
                    let _ = conn.execute(
                        "INSERT OR IGNORE INTO file_tags (file_id, tag_id, origin) \
                         VALUES (?1, ?2, 'source-site')",
                        params![file_id, tag_id],
                    );
                }
            }
        }
    }

    // Чистим временную папку gallery-dl
    if let Some(d) = tmp_dir {
        let _ = std::fs::remove_dir_all(d);
    }
    Ok((imported, dupes))
}
