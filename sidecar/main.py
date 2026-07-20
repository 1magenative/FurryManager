#!/usr/bin/env python3
"""FurryManager ML-сайдкар: CLIP-эмбеддинги (поиск текстом) + WD14-автотеги.

Протокол: JSON-строки через stdin/stdout, по одной на запрос/ответ:
  → {"id": 1, "method": "embed_text", "params": {"text": "red fox"}}
  ← {"id": 1, "result": [0.01, ...]}
Все логи и чужой вывод уходят в stderr, stdout — только протокол.
Модели скачиваются при первом использовании в кэш HuggingFace (~1 ГБ суммарно).
"""
import json
import os
import sys
import traceback
import warnings

os.environ.setdefault("HF_HUB_DISABLE_PROGRESS_BARS", "1")
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")
# huggingface_hub ругается на собственную же переменную выше — глушим
warnings.filterwarnings("ignore", module="huggingface_hub")

# Файлы с оборванным хвостом (несколько байт) читаем как есть, а не падаем:
# лучше проиндексировать 99.99% картинки, чем ничего.
try:
    from PIL import ImageFile
    ImageFile.LOAD_TRUNCATED_IMAGES = True
except Exception:
    pass

# Приватность: после первого скачивания моделей полностью запрещаем huggingface_hub
# ходить в сеть (иначе он проверяет обновления при каждой загрузке модели).
# Маркер ставится после успешной загрузки обеих моделей. Env-переменную нужно
# выставить ДО импорта hf-библиотек — поэтому здесь, в самом начале.
_MARKER = os.path.join(
    os.environ.get("APPDATA", os.path.expanduser("~")), "FurryManager", "models_ready"
)
if os.path.exists(_MARKER):
    os.environ.setdefault("HF_HUB_OFFLINE", "1")


def _mark_models_ready():
    """Обе модели загрузились — дальше работаем строго офлайн."""
    try:
        os.makedirs(os.path.dirname(_MARKER), exist_ok=True)
        with open(_MARKER, "w") as f:
            f.write("ok")
    except Exception:
        pass

# На русской Windows пайпы по умолчанию в cp1251 — принудительно UTF-8,
# иначе Rust падает с "stream did not contain valid UTF-8" на кириллице.
try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

REAL_STDOUT = sys.stdout
sys.stdout = sys.stderr  # чтобы чужие print не ломали протокол


def send(obj):
    REAL_STDOUT.write(json.dumps(obj, ensure_ascii=False) + "\n")
    REAL_STDOUT.flush()


def log(*a):
    print("[sidecar]", *a, file=sys.stderr, flush=True)


# ---------- CLIP (fastembed, ONNX) ----------
_clip_img = None
_clip_txt = None


def clip_models():
    global _clip_img, _clip_txt
    if _clip_img is None:
        log("Загружаю CLIP (первый раз — скачивание ~600 МБ)…")
        from fastembed import ImageEmbedding, TextEmbedding
        _clip_img = ImageEmbedding("Qdrant/clip-ViT-B-32-vision")
        _clip_txt = TextEmbedding("Qdrant/clip-ViT-B-32-text")
        log("CLIP готов")
        if _wd is not None:
            _mark_models_ready()
    return _clip_img, _clip_txt


# ---------- WD14-теггер (onnxruntime) ----------
_wd = None  # (session, input_name, size, tags: [(name, category)])
WD_REPO = "SmilingWolf/wd-swinv2-tagger-v3"


def wd_model():
    global _wd
    if _wd is None:
        log("Загружаю WD14-теггер (первый раз — скачивание ~450 МБ)…")
        import csv
        import onnxruntime as ort
        from huggingface_hub import hf_hub_download

        model_path = hf_hub_download(WD_REPO, "model.onnx")
        csv_path = hf_hub_download(WD_REPO, "selected_tags.csv")
        session = ort.InferenceSession(model_path, providers=["CPUExecutionProvider"])
        inp = session.get_inputs()[0]
        size = inp.shape[1] if isinstance(inp.shape[1], int) else 448
        tags = []
        with open(csv_path, newline="", encoding="utf-8") as f:
            for row in csv.DictReader(f):
                tags.append((row["name"], int(row["category"])))
        _wd = (session, inp.name, size, tags)
        log("WD14 готов,", len(tags), "тегов, вход", size)
        if _clip_img is not None:
            _mark_models_ready()
    return _wd


def prep_image(path, size):
    """Препроцессинг WD14: белый фон, квадрат с полями, BGR float32."""
    import numpy as np
    from PIL import Image

    img = Image.open(path).convert("RGBA")
    bg = Image.new("RGBA", img.size, (255, 255, 255, 255))
    img = Image.alpha_composite(bg, img).convert("RGB")
    side = max(img.size)
    canvas = Image.new("RGB", (side, side), (255, 255, 255))
    canvas.paste(img, ((side - img.width) // 2, (side - img.height) // 2))
    canvas = canvas.resize((size, size), Image.BICUBIC)
    arr = np.asarray(canvas, dtype=np.float32)[:, :, ::-1]  # RGB -> BGR
    return arr


# ---------- Методы ----------
def m_embed_images(params):
    imodel, _ = clip_models()
    paths = params["paths"]
    try:
        vecs = list(imodel.embed(paths))
        return [[float(x) for x in v] for v in vecs]
    except Exception:
        log("батч не прошёл, пробую по одному")
    out = []
    for p in paths:
        try:
            v = list(imodel.embed([p]))[0]
            out.append([float(x) for x in v])
        except Exception as e:
            log("embed fail:", p, repr(e))
            out.append(None)
    return out


def m_embed_text(params):
    _, tmodel = clip_models()
    v = list(tmodel.embed([params["text"]]))[0]
    return [float(x) for x in v]


def m_tag_images(params):
    import numpy as np

    session, inp_name, size, tags = wd_model()
    res = []
    for p in params["paths"]:
        try:
            arr = prep_image(p, size)
            probs = session.run(None, {inp_name: np.expand_dims(arr, 0)})[0][0]
            items = []
            for (name, cat), prob in zip(tags, probs):
                pr = float(prob)
                if pr >= 0.2:
                    items.append({"name": name, "category": cat, "score": round(pr, 3)})
            res.append(items)
        except Exception as e:
            log("tag fail:", p, repr(e))
            res.append([])
    return res


def m_warmup(_params):
    clip_models()
    wd_model()
    _mark_models_ready()
    return {"ok": True}


# ---------- Скачивание через gallery-dl (e621, FurAffinity, Twitter и ~300 сайтов) ----------
E621_NS = {"artist": None, "character": "character", "species": "species",
           "copyright": "series", "general": "", "lore": "", "meta": None}


def _extract_meta(meta):
    """Достаём артиста, название и теги из метаданных gallery-dl (формат зависит от сайта)."""
    artist = None
    tags = []  # [(namespace, name)]
    t = meta.get("tags")
    if isinstance(t, dict):  # e621: {artist: [...], species: [...], ...}
        for cat, names in t.items():
            ns = E621_NS.get(cat, "")
            if not isinstance(names, list):
                continue
            if cat == "artist" and names:
                artist = names[0]
                continue
            if ns is None:
                continue
            tags.extend((ns, str(n).replace("_", " ")) for n in names)
    elif isinstance(t, list):  # FurAffinity и др.: плоский список
        tags.extend(("", str(n).replace("_", " ")) for n in t)
    if not artist:
        for key in ("artist", "uploader", "user", "author"):
            v = meta.get(key)
            if isinstance(v, dict):
                v = v.get("name") or v.get("nick")
            if v:
                artist = str(v)
                break
    title = meta.get("title") or meta.get("content") or ""
    url = meta.get("post_url") or meta.get("file_url") or None
    return artist, str(title)[:200], tags, url


def m_download(params):
    import subprocess
    import tempfile

    url = params["url"]
    tmp = tempfile.mkdtemp(prefix="fm_dl_")
    cmd = [sys.executable, "-m", "gallery_dl", "--write-metadata", "-D", tmp]
    # Куки залогиненного браузера — для возрастного контента FA/e621
    browser = (params.get("cookies") or "").strip()
    if browser:
        cmd += ["--cookies-from-browser", browser]
    cmd.append(url)
    log("gallery-dl:", url)
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=900)

    media = [f for f in sorted(os.listdir(tmp)) if not f.endswith(".json")]
    if not media:
        tail = (r.stderr or r.stdout or "").strip().splitlines()[-3:]
        raise RuntimeError("gallery-dl не скачал файлы: " + " | ".join(tail))

    items = []
    for f in media:
        path = os.path.join(tmp, f)
        meta = {}
        mj = path + ".json"
        if os.path.exists(mj):
            try:
                with open(mj, encoding="utf-8") as fh:
                    meta = json.load(fh)
            except Exception as e:
                log("meta parse fail:", mj, repr(e))
        artist, title, tags, post_url = _extract_meta(meta)
        items.append({
            "path": path,
            "url": post_url or url,
            "site": meta.get("category", ""),
            "artist": artist,
            "title": title,
            "tags": tags,
        })
    log("скачано файлов:", len(items))
    return {"dir": tmp, "items": items}


METHODS = {
    "ping": lambda p: {"ok": True},
    "warmup": m_warmup,
    "embed_images": m_embed_images,
    "embed_text": m_embed_text,
    "tag_images": m_tag_images,
    "download": m_download,
}


def main():
    log("Сайдкар запущен, Python", sys.version.split()[0])
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        rid = None
        try:
            req = json.loads(line)
            rid = req.get("id")
            fn = METHODS.get(req.get("method"))
            if fn is None:
                send({"id": rid, "error": "нет метода " + str(req.get("method"))})
                continue
            send({"id": rid, "result": fn(req.get("params") or {})})
        except Exception as e:
            traceback.print_exc()
            send({"id": rid, "error": str(e)})


if __name__ == "__main__":
    main()
