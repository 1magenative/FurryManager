use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
pub struct SimilarCard {
    pub id: i64,
    pub kind: String,
    pub ext: String,
    pub width: i64,
    pub height: i64,
    pub size: i64,
    pub path: String,
    pub thumb: Option<String>,
    pub distance: u32,
    pub similarity: u32, // 0–100, для отображения
}

/// Локальный поиск похожих по dHash (расстояние Хэмминга).
pub fn similar_local(
    conn: &Connection,
    root: &Path,
    file_id: i64,
    limit: usize,
) -> Result<Vec<SimilarCard>, String> {
    let target: Option<i64> = conn
        .query_row("SELECT phash FROM files WHERE id = ?1", [file_id], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();

    let target = target.ok_or("Для этого файла нет perceptual hash — для видео локальный поиск похожих появится в v0.2 (CLIP)")?;

    let mut stmt = conn
        .prepare(
            "SELECT id, kind, ext, width, height, size, rel_path, thumb_rel, phash
             FROM files WHERE phash IS NOT NULL AND id <> ?1",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([file_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut out: Vec<SimilarCard> = Vec::new();
    for row in rows.flatten() {
        let (id, kind, ext, width, height, size, rel_path, thumb_rel, phash) = row;
        let distance = ((target as u64) ^ (phash as u64)).count_ones();
        if distance <= 26 {
            out.push(SimilarCard {
                id,
                kind,
                ext,
                width,
                height,
                size,
                path: root.join(&rel_path).to_string_lossy().into_owned(),
                thumb: thumb_rel.map(|t| root.join(t).to_string_lossy().into_owned()),
                distance,
                similarity: ((64 - distance) * 100 / 64),
            });
        }
    }
    out.sort_by_key(|c| c.distance);
    out.truncate(limit);
    Ok(out)
}

#[derive(Serialize)]
pub struct WebMatch {
    pub platform: String,
    pub url: String,
    pub score: f64, // 0–1
    pub match_kind: String,
    pub thumb: Option<String>,
    pub credits: Option<String>,
}

/// Обратный поиск в интернете через Fluffle.xyz (FA, e621, Weasyl, Twitter, Itaku).
pub async fn fluffle_search(image_path: PathBuf) -> Result<Vec<WebMatch>, String> {
    let bytes = std::fs::read(&image_path)
        .map_err(|e| format!("Не удалось прочитать файл для поиска: {e}"))?;

    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name("query.jpg")
        .mime_str("image/jpeg")
        .map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("limit", "32")
        .text("includeNsfw", "true");

    let client = reqwest::Client::new();
    let resp = client
        .post("https://api.fluffle.xyz/v1/search")
        .header(reqwest::header::USER_AGENT, "FurryManager/0.1 (local reference manager)")
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("Нет соединения с Fluffle.xyz: {e}"))?;

    let status = resp.status();
    let val: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Некорректный ответ Fluffle: {e}"))?;

    if !status.is_success() {
        let msg = val
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("неизвестная ошибка");
        return Err(format!("Fluffle ответил ошибкой ({status}): {msg}"));
    }

    let empty = Vec::new();
    let results = val
        .get("results")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);

    let mut out = Vec::new();
    for r in results {
        let url = r
            .get("location")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if url.is_empty() {
            continue;
        }
        let credits = r
            .get("credits")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|c| c.get("name"))
            .and_then(|n| n.as_str())
            .map(|s| s.to_string());
        out.push(WebMatch {
            platform: r
                .get("platform")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            url,
            score: r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0),
            match_kind: r
                .get("match")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            thumb: r
                .get("thumbnail")
                .and_then(|t| t.get("location"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            credits,
        });
    }
    // Сначала точные совпадения, потом по score.
    out.sort_by(|a, b| {
        let rank = |m: &str| match m {
            "exact" => 0,
            "tossUp" => 1,
            "alternative" => 1,
            _ => 2,
        };
        rank(&a.match_kind)
            .cmp(&rank(&b.match_kind))
            .then(b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
    });
    Ok(out)
}
