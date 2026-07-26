//! Конвертация HEIC/HEIF (фото iPhone) в JPEG через Python + pillow-heif.
//! Rust-крейт image HEIC не читает, а libheif на MSVC — отдельная боль;
//! Python в проекте и так нужен для ИИ, pillow-heif ставится вместе с ним.

use crate::commands::load_config;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const PY: &str = r#"
import sys, json, os, tempfile
from PIL import Image, ImageOps
from pillow_heif import register_heif_opener
register_heif_opener()
try:
    from pillow_heif import register_avif_opener
    register_avif_opener()  # AVIF тем же движком (веб-арты с iPhone часто в нём)
except Exception:
    pass
out = {}
for p in sys.argv[1:]:
    try:
        im = Image.open(p)
        im = ImageOps.exif_transpose(im)  # поворот из EXIF, иначе фото лягут боком
        if im.mode != "RGB":
            im = im.convert("RGB")
        fd, tmp = tempfile.mkstemp(suffix=".jpg", prefix="fm_heic_")
        os.close(fd)
        im.save(tmp, "JPEG", quality=95)
        out[p] = tmp
    except Exception:
        out[p] = None
print(json.dumps(out))
"#;

/// Конвертирует один HEIC во временный JPEG. Вызывающий удаляет файл после импорта.
pub fn convert_one(src: &Path) -> Result<PathBuf, String> {
    let map = convert_batch(&[src.to_path_buf()])?;
    map.get(src)
        .and_then(|v| v.clone())
        .ok_or_else(|| "HEIC не сконвертировался (повреждённый файл?)".to_string())
}

/// Пакетная конвертация одним запуском Python.
/// Ключ — исходный путь, значение — временный JPEG (None = файл не прочитался).
pub fn convert_batch(paths: &[PathBuf]) -> Result<HashMap<PathBuf, Option<PathBuf>>, String> {
    if paths.is_empty() {
        return Ok(HashMap::new());
    }
    let mut last_err = String::new();
    for cand in python_candidates() {
        let mut cmd = Command::new(&cand[0]);
        for a in &cand[1..] {
            cmd.arg(a);
        }
        cmd.arg("-c").arg(PY);
        for p in paths {
            cmd.arg(p);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        match cmd.output() {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let line = stdout
                    .lines()
                    .rev()
                    .find(|l| l.trim_start().starts_with('{'))
                    .ok_or("HEIC: пустой ответ конвертера")?;
                let val: serde_json::Value = serde_json::from_str(line.trim())
                    .map_err(|e| format!("HEIC: некорректный ответ конвертера: {e}"))?;
                let mut map = HashMap::new();
                if let Some(obj) = val.as_object() {
                    for (k, v) in obj {
                        map.insert(PathBuf::from(k), v.as_str().map(PathBuf::from));
                    }
                }
                return Ok(map);
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr).to_string();
                if err.contains("No module named") {
                    return Err(
                        "Для HEIC-фото нужен пакет pillow-heif: нажми 🧠 → \
                         «Установить ИИ-компоненты» (или pip install pillow-heif)"
                            .into(),
                    );
                }
                last_err = err.lines().last().unwrap_or("ошибка").to_string();
            }
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!(
        "HEIC: не удалось запустить Python ({last_err}). Нужен Python 3.11+ с pillow-heif"
    ))
}

/// Кандидаты запуска Python — та же логика, что у ИИ-сайдкара.
fn python_candidates() -> Vec<Vec<String>> {
    let cfg = load_config();
    let mut v: Vec<Vec<String>> = Vec::new();
    if let Some(p) = cfg.python_path {
        if !p.trim().is_empty() {
            v.push(vec![p]);
        }
    }
    // В Ubuntu есть только python3; в Windows — python и лаунчер py.
    v.push(vec![crate::commands::default_python().to_string()]);
    if cfg!(windows) {
        v.push(vec!["py".into(), "-3".into()]);
    } else {
        v.push(vec!["python".into()]);
    }
    v
}
