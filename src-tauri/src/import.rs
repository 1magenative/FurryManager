use crate::db;
use image::imageops::FilterType;
use image::DynamicImage;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "jfif", "png", "webp", "bmp"];
const ANIM_EXTS: &[&str] = &["gif"];
const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mov", "m4v"];
const HEIC_EXTS: &[&str] = &["heic", "heif", "avif"]; // конвертируются в JPEG через Python

#[derive(Serialize, Clone)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    pub imported: u64,
    pub duplicates: u64,
    pub errors: u64,
    pub current: String,
}

#[derive(Serialize, Clone, Copy)]
pub struct Summary {
    pub imported: u64,
    pub duplicates: u64,
    pub errors: u64,
    pub total: u64,
}

fn kind_for_ext(ext: &str) -> Option<&'static str> {
    if IMAGE_EXTS.contains(&ext) {
        Some("image")
    } else if ANIM_EXTS.contains(&ext) {
        Some("animation")
    } else if VIDEO_EXTS.contains(&ext) {
        Some("video")
    } else {
        None
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

/// dHash: 64-битный perceptual hash (сравнение яркости соседних пикселей 9×8).
pub fn dhash(img: &DynamicImage) -> i64 {
    let small = img.resize_exact(9, 8, FilterType::Triangle).to_luma8();
    let mut hash: u64 = 0;
    let mut bit = 0u32;
    for y in 0..8u32 {
        for x in 0..8u32 {
            if small.get_pixel(x, y).0[0] > small.get_pixel(x + 1, y).0[0] {
                hash |= 1u64 << bit;
            }
            bit += 1;
        }
    }
    hash as i64
}

fn make_thumb(img: &DynamicImage, dest: &Path) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let thumb = img.thumbnail(400, 400).to_rgb8();
    let mut out = fs::File::create(dest).map_err(|e| e.to_string())?;
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82)
        .encode_image(&thumb)
        .map_err(|e| e.to_string())
}

/// Пробуем сделать превью видео через ffmpeg, если он есть в PATH.
fn make_video_thumb(src: &Path, dest: &Path) -> bool {
    if let Some(dir) = dest.parent() {
        if fs::create_dir_all(dir).is_err() {
            return false;
        }
    }
    Command::new("ffmpeg")
        .args([
            "-y",
            "-ss",
            "0.5",
            "-i",
            &src.to_string_lossy(),
            "-frames:v",
            "1",
            "-vf",
            "scale=400:-2",
            &dest.to_string_lossy(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success() && dest.exists())
        .unwrap_or(false)
}

/// Определяем реальный формат по магическим байтам — расширению доверять нельзя
/// (iOS отдаёт HEIC/AVIF-байты, подписывая их image/jpeg).
pub(crate) fn sniff_kind(path: &Path) -> &'static str {
    use std::io::Read;
    let Ok(mut f) = fs::File::open(path) else { return "missing" };
    let mut buf = [0u8; 24];
    let n = f.read(&mut buf).unwrap_or(0);
    if n == 0 {
        return "empty";
    }
    if n < 12 {
        return "other";
    }
    if buf[0] == 0xFF && buf[1] == 0xD8 && buf[2] == 0xFF {
        return "jpeg";
    }
    if &buf[0..4] == b"\x89PNG" {
        return "png";
    }
    if &buf[0..4] == b"GIF8" {
        return "gif";
    }
    if &buf[0..4] == b"RIFF" && &buf[8..12] == b"WEBP" {
        return "webp";
    }
    if &buf[4..8] == b"ftyp" {
        return match &buf[8..12] {
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heif" | b"mif1" | b"msf1" => "heic",
            b"avif" | b"avis" => "avif",
            _ => "iso-bmff", // mp4/mov и прочие контейнеры
        };
    }
    "other"
}

/// Файл нужно прогнать через Python-конвертер? (HEIC/AVIF по содержимому)
pub(crate) fn needs_convert(path: &Path) -> bool {
    matches!(sniff_kind(path), "heic" | "avif")
}

/// Длительность видео через ffprobe (обычно идёт в комплекте с ffmpeg).
fn video_duration(src: &Path) -> Option<f64> {
    let mut cmd = Command::new("ffprobe");
    cmd.args([
        "-v", "error",
        "-show_entries", "format=duration",
        "-of", "default=noprint_wrappers=1:nokey=1",
        &src.to_string_lossy(),
    ])
    .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// До n кадров видео равномерно по длительности (для индексации: NSFW-сцены
/// редко в начале ролика). Пустой вектор — ffmpeg недоступен/не смог.
pub(crate) fn video_frames(src: &Path, n: usize) -> Vec<PathBuf> {
    let times: Vec<f64> = match video_duration(src) {
        Some(d) if d > 2.0 => (1..=n).map(|i| d * i as f64 / (n as f64 + 1.0)).collect(),
        Some(d) if d > 0.0 => vec![d / 2.0],
        _ => vec![0.5, 5.0, 15.0].into_iter().take(n).collect(), // длительность неизвестна
    };
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut out = Vec::new();
    for (i, t) in times.iter().enumerate() {
        let mut tmp = std::env::temp_dir();
        tmp.push(format!("fm_vidx_{ts}_{i}.jpg"));
        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-y",
            "-ss", &format!("{t:.2}"),
            "-i", &src.to_string_lossy(),
            "-frames:v", "1",
            "-vf", "scale=448:-2",
            &tmp.to_string_lossy(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let ok = cmd
            .status()
            .map(|s| s.success() && fs::metadata(&tmp).map(|m| m.len() > 0).unwrap_or(false))
            .unwrap_or(false);
        if ok {
            out.push(tmp);
        } else {
            let _ = fs::remove_file(&tmp);
        }
    }
    out
}

fn is_supported(p: &Path) -> bool {
    p.extension()
        .and_then(|s| s.to_str())
        .map(|s| {
            let e = s.to_lowercase();
            kind_for_ext(&e).is_some() || HEIC_EXTS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

/// Импорт списка путей (папки раскрываются рекурсивно, файлы берутся как есть).
/// Открывает собственное соединение с БД (WAL позволяет параллельное чтение из UI).
pub fn run(
    root: &Path,
    inputs: &[PathBuf],
    in_place: bool,
    lossless: bool,
    honor_deleted: bool,
    on_progress: impl Fn(&Progress),
) -> Result<Summary, String> {
    let conn = db::open(&root.join("library.db")).map_err(|e| e.to_string())?;

    // Сначала собираем список — чтобы знать total для прогресс-бара.
    let mut entries: Vec<PathBuf> = Vec::new();
    for input in inputs {
        if input.is_dir() {
            entries.extend(
                WalkDir::new(input)
                    .follow_links(false)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file())
                    .map(|e| e.into_path())
                    .filter(|p| is_supported(p)),
            );
        } else if input.is_file() && is_supported(input) {
            entries.push(input.clone());
        }
    }

    let total = entries.len() as u64;
    let (mut imported, mut duplicates, mut errors) = (0u64, 0u64, 0u64);

    for (i, path) in entries.iter().enumerate() {
        match import_one(&conn, root, path, in_place, lossless, honor_deleted) {
            Ok((_, true)) => imported += 1,
            Ok((_, false)) => duplicates += 1,
            Err(_) => errors += 1,
        }
        let done = i as u64 + 1;
        if done % 5 == 0 || done == total {
            on_progress(&Progress {
                done,
                total,
                imported,
                duplicates,
                errors,
                current: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            });
        }
    }

    // Копим счётчик сэкономленных дубликатов — только для явных импортов.
    // Watcher (honor_deleted) при каждом запуске пересканирует папки целиком
    // и раздувал бы счётчик уже имеющимися файлами.
    if !honor_deleted && duplicates > 0 {
        let _ = conn.execute(
            "INSERT INTO meta(key, value) VALUES('dupes_skipped', ?1)
             ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + ?1",
            params![duplicates as i64],
        );
    }

    Ok(Summary {
        imported,
        duplicates,
        errors,
        total,
    })
}

/// Lossless-конвертация PNG → WebP (VP8L). Возвращает Ok при успехе.
fn convert_png_to_webp(src: &Path, dest: &Path) -> Result<(), String> {
    let img = image::open(src).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let mut out = fs::File::create(dest).map_err(|e| e.to_string())?;
    image::codecs::webp::WebPEncoder::new_lossless(&mut out)
        .encode(rgba.as_raw(), w, h, image::ColorType::Rgba8)
        .map_err(|e| e.to_string())
}

/// Починка битого импорта: запись `id` перенастраивается на сконвертированный JPEG
/// (contents `conv`). Используется для HEIC-файлов, сохранённых с неверным расширением.
/// Коллекции/теги/NSFW сохраняются (id не меняется).
pub(crate) fn replace_with_converted(
    conn: &Connection,
    root: &Path,
    id: i64,
    old_abs: &Path,
    conv: &Path,
    storage: Option<&str>,
) -> Result<(), String> {
    let hash = sha256_of(conv).map_err(|e| e.to_string())?;

    // Такой контент уже есть под другим id? Битую запись просто убираем.
    let dup: Option<i64> = conn
        .query_row(
            "SELECT id FROM files WHERE sha256 = ?1 AND id <> ?2",
            params![hash, id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if dup.is_some() {
        conn.execute("DELETE FROM files WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
        if storage != Some("inplace") {
            let _ = fs::remove_file(old_abs);
        }
        return Ok(());
    }

    let shard = &hash[..2];
    let rel = format!("store/{shard}/{hash}.jpg");
    let abs = root.join(&rel);
    if let Some(d) = abs.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    fs::copy(conv, &abs).map_err(|e| e.to_string())?;
    let size = fs::metadata(&abs).map(|m| m.len() as i64).unwrap_or(0);

    let (mut w, mut h, mut ph, mut thumb) = (0i64, 0i64, None::<i64>, None::<String>);
    if let Ok(img) = image::open(&abs) {
        w = img.width() as i64;
        h = img.height() as i64;
        ph = Some(dhash(&img));
        let t = format!("thumbs/{shard}/{hash}.jpg");
        if make_thumb(&img, &root.join(&t)).is_ok() {
            thumb = Some(t);
        }
    }

    conn.execute(
        "UPDATE files SET sha256 = ?1, ext = 'jpg', size = ?2, width = ?3, height = ?4, \
         phash = ?5, rel_path = ?6, thumb_rel = ?7, storage = 'store' WHERE id = ?8",
        params![hash, size, w, h, ph, rel, thumb, id],
    )
    .map_err(|e| e.to_string())?;
    // Контент сменился — старый (пустой) вектор индексации больше не актуален
    let _ = conn.execute("DELETE FROM clip_vectors WHERE file_id = ?1", [id]);

    // Старую копию в store убираем (in-place оригинал пользователя не трогаем).
    if storage != Some("inplace") && old_abs != abs {
        let _ = fs::remove_file(old_abs);
    }
    Ok(())
}

/// Пересчёт метаданных записи по её существующему файлу (размеры/phash/превью).
/// Ok(true) — удалось декодировать и обновить; Ok(false) — файл не декодируется.
pub(crate) fn refresh_media_meta(
    conn: &Connection,
    root: &Path,
    id: i64,
    abs: &Path,
) -> Result<bool, String> {
    let Ok(img) = image::open(abs) else {
        return Ok(false);
    };
    let (w, h) = (img.width() as i64, img.height() as i64);
    let ph = dhash(&img);
    let sha: String = conn
        .query_row("SELECT sha256 FROM files WHERE id = ?1", [id], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let shard = &sha[..2];
    let t = format!("thumbs/{shard}/{sha}.jpg");
    let thumb = if make_thumb(&img, &root.join(&t)).is_ok() {
        Some(t)
    } else {
        None
    };
    conn.execute(
        "UPDATE files SET width = ?1, height = ?2, phash = ?3, thumb_rel = ?4 WHERE id = ?5",
        params![w, h, ph, thumb, id],
    )
    .map_err(|e| e.to_string())?;
    // Прошлая индексация шла по битому файлу — пусть пройдёт заново
    let _ = conn.execute("DELETE FROM clip_vectors WHERE file_id = ?1", [id]);
    Ok(true)
}

/// Ok((id, true)) — импортирован, Ok((id, false)) — дубликат (id существующего)
/// или пропуск по «надгробию» (id = 0).
/// in_place = true: оригинал не копируется, в rel_path хранится абсолютный путь.
/// lossless = true: PNG при копировании пережимается в WebP без потерь.
/// honor_deleted = true (watcher): не реимпортировать то, что пользователь удалил;
/// false (ручной импорт): наоборот, снимает «надгробие» — явное решение вернуть.
pub fn import_one(
    conn: &Connection,
    root: &Path,
    src: &Path,
    in_place: bool,
    lossless: bool,
    honor_deleted: bool,
) -> Result<(i64, bool), String> {
    let ext = src
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .ok_or("нет расширения")?;

    // HEIC/HEIF (фото iPhone): конвертируем во временный JPEG и импортируем его.
    // Оригинал не трогаем; in-place для HEIC не имеет смысла — в библиотеку
    // попадает jpg-копия (поэтому форсим обычный режим).
    // ВАЖНО: проверяем и содержимое — iOS иногда отдаёт HEIC/AVIF-байты
    // с типом image/jpeg, расширению верить нельзя.
    if HEIC_EXTS.contains(&ext.as_str()) || needs_convert(src) {
        let conv = crate::heic::convert_one(src)?;
        let res = import_one(conn, root, &conv, false, lossless, honor_deleted);
        let _ = fs::remove_file(&conv);
        return res;
    }

    let kind = kind_for_ext(&ext).ok_or("неподдерживаемый тип")?;

    let hash = sha256_of(src).map_err(|e| e.to_string())?;

    let exists: Option<i64> = conn
        .query_row("SELECT id FROM files WHERE sha256 = ?1", [&hash], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some(id) = exists {
        return Ok((id, false));
    }

    // «Надгробия» удалённых артов: watcher пропускает, ручной импорт — снимает запрет.
    if honor_deleted {
        let dead: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM deleted_hashes WHERE sha256 = ?1",
                [&hash],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if dead.is_some() {
            return Ok((0, false)); // пользователь удалял этот арт — не возвращаем
        }
    } else {
        let _ = conn.execute("DELETE FROM deleted_hashes WHERE sha256 = ?1", [&hash]);
    }

    let shard = &hash[..2];

    // Где лежит медиа для обработки (превью/размеры), что писать в rel_path и с каким ext.
    let (rel_path, media_src, storage, stored_ext): (String, PathBuf, &str, String) = if in_place {
        // Оригинал остаётся на месте; храним его абсолютный путь.
        (
            src.to_string_lossy().into_owned(),
            src.to_path_buf(),
            "inplace",
            ext.clone(),
        )
    } else if lossless && ext == "png" {
        // Пробуем pngWebP без потерь; при неудаче — обычная копия.
        let rel = format!("store/{shard}/{hash}.webp");
        let abs_dest = root.join(&rel);
        if let Some(dir) = abs_dest.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        match convert_png_to_webp(src, &abs_dest) {
            Ok(()) => (rel, abs_dest, "store", "webp".to_string()),
            Err(_) => {
                let rel = format!("store/{shard}/{hash}.{ext}");
                let abs_dest = root.join(&rel);
                fs::copy(src, &abs_dest).map_err(|e| e.to_string())?;
                (rel, abs_dest, "store", ext.clone())
            }
        }
    } else {
        let rel = format!("store/{shard}/{hash}.{ext}");
        let abs_dest = root.join(&rel);
        if let Some(dir) = abs_dest.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        fs::copy(src, &abs_dest).map_err(|e| e.to_string())?;
        (rel, abs_dest, "store", ext.clone())
    };
    let size = fs::metadata(&media_src).map(|m| m.len() as i64).unwrap_or(0);

    let (mut width, mut height, mut phash, mut thumb_rel) = (0i64, 0i64, None::<i64>, None::<String>);

    if kind == "image" || kind == "animation" {
        // Для GIF image::open даёт первый кадр — этого достаточно.
        if let Ok(img) = image::open(&media_src) {
            width = img.width() as i64;
            height = img.height() as i64;
            phash = Some(dhash(&img));
            let t_rel = format!("thumbs/{shard}/{hash}.jpg");
            if make_thumb(&img, &root.join(&t_rel)).is_ok() {
                thumb_rel = Some(t_rel);
            }
        }
    } else {
        let t_rel = format!("thumbs/{shard}/{hash}.jpg");
        if make_video_thumb(&media_src, &root.join(&t_rel)) {
            thumb_rel = Some(t_rel);
        }
    }

    conn.execute(
        "INSERT INTO files (sha256, phash, ext, size, width, height, kind, rel_path, thumb_rel, storage, imported_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![hash, phash, stored_ext, size, width, height, kind, rel_path, thumb_rel, storage, now()],
    )
    .map_err(|e| e.to_string())?;

    Ok((conn.last_insert_rowid(), true))
}
