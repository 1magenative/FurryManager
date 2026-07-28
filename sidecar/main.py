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


# id текущего запроса — чтобы промежуточные сообщения о ходе работы
# (например, сколько страниц комикса уже скачано) можно было привязать к нему.
CURRENT_ID = None


def progress(data):
    """Промежуточный отчёт о ходе выполнения; ответ на запрос будет отдельно."""
    if CURRENT_ID is not None:
        send({"id": CURRENT_ID, "progress": data})


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


def _gallery_dl_call(args):
    """Вызов gallery-dl В ЭТОМ ЖЕ процессе.

    Раньше вызывали `sys.executable -m gallery_dl`. В собранном PyInstaller
    бинарнике sys.executable — это сам сайдкар, а не python: запускался второй
    экземпляр сайдкара, который молча ждал команды на stdin, и скачивание
    «висело» до таймаута. Поэтому дёргаем точку входа напрямую.

    sys.stdout подменяем на stderr: gallery-dl печатает прогресс в stdout, а
    ответы протокола идут через REAL_STDOUT (сохранён при старте), так что
    подмена безопасна.
    """
    import contextlib
    import logging
    import gallery_dl

    # Вызов в том же процессе оставляет состояние от прошлого раза: gallery-dl
    # каждый раз вешает новый обработчик логов (строки начинали дублироваться)
    # и держит прежний конфиг. Чистим перед запуском.
    try:
        from gallery_dl import config as gdl_config
        gdl_config.clear()
    except Exception:
        pass
    root_logger = logging.getLogger()
    for h in root_logger.handlers[:]:
        root_logger.removeHandler(h)

    old_argv, old_stdin, old_stdout = sys.argv, sys.stdin, sys.stdout
    sys.argv = ["gallery-dl"] + list(args)

    # gallery-dl при запуске перенастраивает кодировку стандартных потоков
    # (stdout/stderr/stdin). Наш stdin уже читается основным циклом, а
    # перенастройка прочитанного потока запрещена — отсюда была ошибка
    # «not possible to set the encoding … after the first read».
    # Поэтому на время вызова подсовываем свежие потоки:
    #   stdin  — пустой (gallery-dl ввод не нужен),
    #   stdout — отдельная обёртка над stderr (в наш протокол он не попадёт).
    # Основной цикл держит ссылку на настоящий stdin, подмена ему не мешает.
    devnull = open(os.devnull, "r", encoding="utf-8")
    try:
        fresh_out = os.fdopen(os.dup(2), "w", encoding="utf-8", errors="replace")
    except Exception:
        fresh_out = sys.stderr

    code = 0
    try:
        sys.stdin = devnull
        with contextlib.redirect_stdout(fresh_out):
            try:
                gallery_dl.main()
            except SystemExit as e:
                code = e.code if isinstance(e.code, int) else 0
    finally:
        sys.stdin = old_stdin
        sys.stdout = old_stdout
        for f in (devnull, fresh_out):
            if f is not sys.stderr:
                try:
                    f.close()
                except Exception:
                    pass
        sys.argv = old_argv
    return code


def _run_gallery_dl(args, timeout=1800):
    """То же, но со сторожем по времени: зависшее скачивание не должно
    блокировать сайдкар навсегда (иначе встают и ИИ, и вся очередь)."""
    import threading

    box = {"code": 0, "err": None}

    def work():
        try:
            box["code"] = _gallery_dl_call(args)
        except BaseException as e:  # noqa: BLE001 — любую ошибку отдаём наверх
            box["err"] = repr(e)

    t = threading.Thread(target=work, daemon=True)
    t.start()
    t.join(timeout)
    if t.is_alive():
        raise RuntimeError(
            "gallery-dl не ответил за {} мин — прервано. Проверь ссылку и связь.".format(
                timeout // 60
            )
        )
    if box["err"]:
        raise RuntimeError("gallery-dl: " + box["err"])
    return box["code"]


def m_download(params):
    import tempfile

    url = params["url"]
    comic = bool(params.get("comic"))
    tmp = tempfile.mkdtemp(prefix="fm_dl_")
    args = ["--write-metadata", "-D", tmp]
    # Куки залогиненного браузера — для возрастного контента FA/e621
    browser = (params.get("cookies") or "").strip()
    if browser:
        args += ["--cookies-from-browser", browser]
    # Порядок страниц НЕ задаём через --filename: у части сайтов (в т.ч. e621)
    # нет полей {num}/{filename}, и вся закачка пула молча ломается. Берём
    # позицию из метаданных ниже и сортируем сами.
    args.append(url)
    log("gallery-dl:", url, "(комикс)" if comic else "")

    # Пока идёт скачивание, раз в 2 секунды сообщаем, сколько файлов уже легло:
    # у больших комиксов сбор занимает минуты, и без этого непонятно, живо ли.
    import threading as _th

    stop = _th.Event()

    def _watch():
        last = -1
        while not stop.is_set():
            try:
                n = len([f for f in os.listdir(tmp) if not f.endswith(".json")])
                if n != last:
                    last = n
                    progress({"files": n, "comic": comic})
            except Exception:
                pass
            stop.wait(2)

    watcher = _th.Thread(target=_watch, daemon=True)
    watcher.start()
    try:
        code = _run_gallery_dl(args)
    finally:
        stop.set()

    media = [f for f in sorted(os.listdir(tmp)) if not f.endswith(".json")]
    if not media:
        raise RuntimeError(
            "gallery-dl не скачал файлы (код {}). Проверь ссылку; для взрослого "
            "контента укажи браузер с куками в настройках.".format(code)
        )

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
        pool = meta.get("pool")
        if isinstance(pool, dict):
            pool_name = pool.get("name") or ""
        elif isinstance(pool, list) and pool:
            first = pool[0]
            pool_name = first.get("name", "") if isinstance(first, dict) else ""
        else:
            pool_name = ""
        # Позиция страницы в комиксе: сначала явные поля gallery-dl, затем
        # место поста в списке пула, иначе — порядок имени файла.
        pos = meta.get("num")
        if pos is None and isinstance(pool, dict):
            ids = pool.get("post_ids")
            pid = meta.get("id")
            if isinstance(ids, list) and pid in ids:
                pos = ids.index(pid)
        items.append({
            "path": path,
            "pos": pos if isinstance(pos, int) else None,
            "pool": str(pool_name).replace("_", " ").strip(),
            "url": post_url or url,
            "site": meta.get("category", ""),
            "artist": artist,
            "title": title,
            "tags": tags,
        })
    # Для комикса выстраиваем страницы по позиции (у кого её нет — в конец)
    if comic:
        items.sort(key=lambda it: (it["pos"] is None, it["pos"] if it["pos"] is not None else 0))
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
            global CURRENT_ID
            CURRENT_ID = rid
            try:
                send({"id": rid, "result": fn(req.get("params") or {})})
            finally:
                CURRENT_ID = None
        except Exception as e:
            traceback.print_exc()
            send({"id": rid, "error": str(e)})


if __name__ == "__main__":
    main()
