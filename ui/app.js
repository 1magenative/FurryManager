// ===== FurryManager UI =====
const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const tauriDialog = window.__TAURI__.dialog;
const opener = window.__TAURI__.opener;

const $ = (id) => document.getElementById(id);

const MEDIA_EXTS = ["jpg", "jpeg", "jfif", "png", "webp", "bmp", "gif", "mp4", "webm", "mov", "m4v", "heic", "heif"];

const state = {
  mode: "simple",
  query: "",
  offset: 0,
  limit: 120,
  loading: false,
  reachedEnd: false,
  cards: [],             // загруженные карточки по порядку
  viewerIndex: -1,       // индекс открытого файла в cards
  collection: null,      // фильтр по коллекции (id или null = все)
  collections: [],       // список коллекций
  viewerCollections: [], // id коллекций открытого файла
  viewerTags: [],        // теги открытого файла (для «Похожее по тегам»)
  selected: new Set(),   // выбранные file id (Ctrl+клик)
  lastIndex: null,       // последний кликнутый индекс (для Shift)
  smart: false,          // умный (семантический) поиск
  smartAll: [],          // полные результаты умного поиска (со score)
  strict: false,         // «только точные» в умном поиске
  nsfwSplit: false,      // включены ли вкладки SFW/NSFW
  nsfw: null,            // текущий фильтр: null = все, 0 = SFW, 1 = NSFW
  defaultNsfw: null,     // фильтр при запуске (когда включены вкладки)
  recent: false,         // фильтр «недавно просмотренные»
  similarTo: null,       // режим «похожие на арт» в галерее (карточка-образец)
  simCards: [],          // последние результаты «похожих в библиотеке»
  simFor: null,          // по какому арту искали похожих
  lossless: false,       // сжимать PNG → WebP без потерь при копировании
  lan: false,            // приём артов с телефона по локальной сети
  hideNsfw: false,       // режим тотального скрытия NSFW (коллекции + фильтр)
  favOnly: false,        // фильтр «только избранное» (★)
  sort: null,            // порядок галереи: null=новые, old/big/small/views/random
  randSeed: 1,           // зерно «случайной» сортировки (стабильный порядок)
  display: null,         // тема, размер миниатюр, водопад, бейджи, компакт
  useTrash: true,        // удалять через корзину
  trashDays: 30,         // через сколько дней чистить корзину (0 = никогда)
  gifAuto: false,        // авто-конвертация коротких видео в GIF
  gifSecs: 5,            // порог длительности для видео→GIF
  gifReplace: false,     // заменять оригинал видео на GIF
  gifFps: 15,            // кадров/сек в GIF
  gifWidth: 480,         // макс. сторона GIF, px
  ready: false,          // init() завершён — до этого автоподгрузку не запускаем
  pendingReset: false,   // сброс пришёл во время загрузки — повторить после

};

// ---------- Утилиты ----------
function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}
function fmtBytes(n) {
  if (n < 1024) return n + " Б";
  const u = ["КБ", "МБ", "ГБ", "ТБ"];
  let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1);
  return n.toFixed(1) + " " + u[i];
}
function mediaSrc(path) { return convertFileSrc(path); }
function parseTag(raw) {
  let ns = "", name = raw.trim();
  const idx = name.indexOf(":");
  if (idx > 0) { ns = name.slice(0, idx); name = name.slice(idx + 1); }
  return { ns, name };
}

// ---------- Режимы ----------
function updatePlaceholder() {
  $("search").placeholder = state.smart
    ? "Умный поиск: red fox in jacket, можно с фильтрами: gif species:fox…"
    : state.mode !== "simple"
      ? "Поиск: раса:лис -эскиз куртка gif…"
      : "Поиск: лис, куртка… (Enter)";
}

// Режимы: simple | user | artist. Старые значения из конфига приводим к новым.
function normMode(m) {
  if (m === "basic") return "simple";
  if (m === "advanced") return "user";
  return (m === "simple" || m === "user" || m === "artist") ? m : "simple";
}
function applyMode(mode) {
  state.mode = normMode(mode);
  document.body.dataset.mode = state.mode;
  updatePlaceholder();
}

// ---------- Настройки отображения (тема, сетка, водопад, бейджи, компакт) ----------
// Это визуальные предпочтения конкретного компьютера — храним локально.
const DISPLAY_DEFAULTS = { theme: "dark", thumb: 200, masonry: false, nobadges: false,
                           compact: false, nocolscroll: true };
function loadDisplay() {
  let saved = {};
  try { saved = JSON.parse(localStorage.getItem("fm_display") || "{}"); } catch {}
  return Object.assign({}, DISPLAY_DEFAULTS, saved || {});
}
function saveDisplay(d) {
  try { localStorage.setItem("fm_display", JSON.stringify(d)); } catch {}
}
function applyDisplay(d) {
  state.display = d;
  document.documentElement.dataset.theme = d.theme || "dark";
  document.documentElement.style.setProperty("--thumb", (d.thumb || 200) + "px");
  document.body.classList.toggle("masonry", !!d.masonry);
  document.body.classList.toggle("nobadges", !!d.nobadges);
  document.body.classList.toggle("compact", !!d.compact);
  document.body.classList.toggle("nocolscroll", !!d.nocolscroll);
}

// ---------- Раскладка галереи ----------
// В обычном режиме — CSS-сетка. В «водопаде» карточки раскладываем по колонкам
// сами (по кругу), иначе браузерные CSS-колонки пустят порядок сверху вниз по
// первой колонке — читать ленту становится неудобно.
let masonrySeq = 0; // сколько карточек уже разложено (для продолжения по кругу)
function isMasonry() { return document.body.classList.contains("masonry"); }
function masonryColCount() {
  const g = $("gallery");
  const w = g.clientWidth || 1200;
  const thumb = (state.display && state.display.thumb) || 200;
  return Math.max(1, Math.floor((w + 12) / (thumb + 12)));
}
function ensureMasonryCols() {
  const g = $("gallery");
  const need = masonryColCount();
  const cols = g.querySelectorAll(":scope > .mcol");
  if (cols.length === need) return;
  // Пересобираем: собираем карточки в исходном порядке и раскладываем заново
  const cards = [...g.querySelectorAll(".card")].sort(
    (a, b) => Number(a.dataset.i) - Number(b.dataset.i));
  g.innerHTML = "";
  for (let i = 0; i < need; i++) {
    g.insertAdjacentHTML("beforeend", `<div class="mcol"></div>`);
  }
  const fresh = g.querySelectorAll(":scope > .mcol");
  masonrySeq = 0;
  cards.forEach((el) => { fresh[masonrySeq % need].appendChild(el); masonrySeq++; });
}
function galleryClear() {
  $("gallery").innerHTML = "";
  masonrySeq = 0;
  if (isMasonry()) ensureMasonryCols();
}
function galleryAppend(html) {
  const g = $("gallery");
  if (!isMasonry()) { g.insertAdjacentHTML("beforeend", html); return; }
  ensureMasonryCols();
  const cols = g.querySelectorAll(":scope > .mcol");
  if (!cols.length) { g.insertAdjacentHTML("beforeend", html); return; }
  const tmp = document.createElement("div");
  tmp.innerHTML = html;
  while (tmp.firstElementChild) {
    cols[masonrySeq % cols.length].appendChild(tmp.firstElementChild);
    masonrySeq++;
  }
}
function gallerySet(html) { galleryClear(); galleryAppend(html); }
// Перерисовать уже загруженные карточки (смена вида/размера миниатюр)
function rerenderGallery() {
  gallerySet(state.cards.map((c, k) => cardHtml(c, k)).join(""));
}
// Число колонок зависит от ширины окна
let resizeTimer = null;
window.addEventListener("resize", () => {
  if (!isMasonry()) return;
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(ensureMasonryCols, 150);
});

// ---------- Настройки ----------
function openSettings() {
  const modeInput = document.querySelector(`input[name="set-mode"][value="${state.mode}"]`);
  if (modeInput) modeInput.checked = true;
  // Отображение
  const d = state.display || loadDisplay();
  const th = document.querySelector(`input[name="set-theme"][value="${d.theme}"]`);
  if (th) th.checked = true;
  $("set-grid-size").value = d.thumb;
  $("set-grid-size-val").textContent = d.thumb + " px";
  $("set-masonry").checked = !!d.masonry;
  $("set-hide-badges").checked = !!d.nobadges;
  $("set-compact").checked = !!d.compact;
  $("set-hide-colscroll").checked = !!d.nocolscroll;
  // Корзина
  $("set-use-trash").checked = state.useTrash !== false;
  $("set-trash-days").value = state.trashDays ?? 30;
  $("trash-days-wrap").classList.toggle("hidden", state.useTrash === false);
  refreshTrashBadge();
  $("set-nsfw-split").checked = state.nsfwSplit;
  $("set-lossless").checked = state.lossless;
  $("set-lan").checked = state.lan;
  $("set-cookies").value = state.browserCookies || "";
  $("set-hide-nsfw").checked = !!state.hideNsfw;
  $("set-gif-auto").checked = !!state.gifAuto;
  $("set-gif-secs").value = state.gifSecs || 5;
  $("set-gif-fps").value = state.gifFps || 15;
  $("set-gif-width").value = state.gifWidth || 480;
  $("set-gif-replace").checked = !!state.gifReplace;
  updateLanSetup();
  refreshTsStatus();
  refreshTgStatus();
  $("set-default-filter").value =
    state.defaultNsfw === 0 ? "0" : state.defaultNsfw === 1 ? "1" : "";
  updateDefaultFilterVis();
  $("ext-token").value = state.httpToken || "";
  $("ext-port").textContent = `Порт: ${state.httpPort || 7877} (приложение должно быть запущено)`;
  renderSettingsCollections();
  renderTagRelations();
  renderWatched();
  renderPinSettings();
  invoke("sidecar_status")
    .then((s) => {
      $("sidecar-status").textContent = s.text;
      // При собранном сайдкаре pip-установка не нужна
      $("btn-ai-install").classList.toggle("hidden", !!s.packaged);
    })
    .catch(() => { $("sidecar-status").textContent = ""; });
  $("app-version").textContent = `FurryManager ${state.appVersion || ""}`;
  $("lib-path").textContent = state.libraryRoot || "не выбрана";
  $("settings").classList.remove("hidden");
}

// Отображение — применяется сразу (кнопка «Сохранить» не нужна).
function onDisplayChange() {
  const d = {
    theme: (document.querySelector('input[name="set-theme"]:checked') || {}).value || "dark",
    thumb: Math.max(120, Math.min(360, Number($("set-grid-size").value) || 200)),
    masonry: $("set-masonry").checked,
    nobadges: $("set-hide-badges").checked,
    compact: $("set-compact").checked,
    nocolscroll: $("set-hide-colscroll").checked,
  };
  $("set-grid-size-val").textContent = d.thumb + " px";
  const prev = state.display || {};
  const relayout = prev.masonry !== d.masonry || prev.thumb !== d.thumb;
  saveDisplay(d);
  applyDisplay(d);
  // Смена вида/размера меняет число колонок — перекладываем уже загруженное
  if (relayout && state.cards.length) rerenderGallery();
}
document.querySelectorAll('input[name="set-theme"]').forEach((el) =>
  el.addEventListener("change", onDisplayChange));
$("set-grid-size").addEventListener("input", onDisplayChange);
["set-masonry", "set-hide-badges", "set-compact", "set-hide-colscroll"].forEach((id) =>
  $(id).addEventListener("change", onDisplayChange));

// ---------- Корзина ----------
async function saveTrashSettings() {
  const useTrash = $("set-use-trash").checked;
  const days = Math.max(0, Math.min(365, Number($("set-trash-days").value) || 0));
  state.useTrash = useTrash;
  state.trashDays = days;
  $("trash-days-wrap").classList.toggle("hidden", !useTrash);
  try { await invoke("set_trash_settings", { useTrash, days }); } catch (e) { alert(e); }
}
$("set-use-trash").addEventListener("change", saveTrashSettings);
$("set-trash-days").addEventListener("change", saveTrashSettings);

async function refreshTrashBadge() {
  try {
    const n = await invoke("trash_count");
    $("trash-badge").textContent = n ? ` (${n})` : "";
  } catch { $("trash-badge").textContent = ""; }
}

let trashItems = [];
const trashMarked = new Set();
async function openTrash() {
  $("trash").classList.remove("hidden");
  $("trash-status").textContent = "Загружаю…";
  $("trash-list").innerHTML = "";
  trashMarked.clear();
  try { trashItems = await invoke("trash_list"); }
  catch (e) { $("trash-status").textContent = "Ошибка: " + e; return; }
  renderTrash();
}
function renderTrash() {
  const days = state.trashDays ?? 30;
  $("trash-status").textContent = trashItems.length
    ? `В корзине: ${trashItems.length}` + (days > 0 ? ` · автоочистка через ${days} дн.` : " · автоочистка выключена")
    : "Корзина пуста";
  $("trash-all").checked = trashItems.length > 0 && trashMarked.size === trashItems.length;
  $("trash-list").innerHTML = trashItems.map((t) => {
    const when = new Date((t.deleted_at || 0) * 1000).toLocaleDateString();
    const img = t.preview
      ? `<img loading="lazy" src="${mediaSrc(t.preview)}" alt=""/>`
      : `<div class="noimg">${t.kind === "video" ? "🎬" : "🖼"}</div>`;
    return `<div class="tr-item ${trashMarked.has(t.id) ? "marked" : ""}" data-id="${t.id}">` +
      `${img}<div class="tr-meta">${esc(t.name || t.sha256.slice(0, 8))}<br/>удалён ${when}</div></div>`;
  }).join("");
  updateTrashBtns();
}
function updateTrashBtns() {
  const n = trashMarked.size;
  $("trash-restore").textContent = `↩ Восстановить (${n})`;
  $("trash-del").textContent = `🗑 Удалить навсегда (${n})`;
  $("trash-restore").disabled = n === 0;
  $("trash-del").disabled = n === 0;
}
$("trash-list").addEventListener("click", (e) => {
  const item = e.target.closest(".tr-item[data-id]");
  if (!item) return;
  const id = Number(item.dataset.id);
  if (trashMarked.has(id)) trashMarked.delete(id); else trashMarked.add(id);
  item.classList.toggle("marked", trashMarked.has(id));
  $("trash-all").checked = trashMarked.size === trashItems.length;
  updateTrashBtns();
});
$("trash-all").addEventListener("change", (e) => {
  trashMarked.clear();
  if (e.target.checked) trashItems.forEach((t) => trashMarked.add(t.id));
  renderTrash();
});
$("trash-restore").addEventListener("click", async () => {
  if (!trashMarked.size) return;
  try {
    const n = await invoke("trash_restore", { ids: [...trashMarked] });
    trashMarked.clear();
    await openTrash();
    refreshStats(); refreshCollections(); loadMore(true); refreshTrashBadge();
    if (n) backgroundIndex(); // вернувшимся артам вернём и превью/индексацию
  } catch (e) { alert(e); }
});
$("trash-del").addEventListener("click", async () => {
  if (!trashMarked.size) return;
  const yes = await tauriDialog.ask(
    `Удалить ${trashMarked.size} арт(ов) НАВСЕГДА? Восстановить будет нельзя.`,
    { title: "FurryManager", kind: "warning" });
  if (!yes) return;
  try {
    await invoke("trash_purge", { ids: [...trashMarked] });
    trashMarked.clear();
    await openTrash();
    refreshTrashBadge();
  } catch (e) { alert(e); }
});
$("trash-empty").addEventListener("click", async () => {
  if (!trashItems.length) return;
  const yes = await tauriDialog.ask(
    "Очистить корзину полностью? Все удалённые арты исчезнут безвозвратно.",
    { title: "FurryManager", kind: "warning" });
  if (!yes) return;
  try {
    await invoke("trash_purge", { ids: [] });
    await openTrash();
    refreshTrashBadge();
  } catch (e) { alert(e); }
});
$("btn-trash").addEventListener("click", openTrash);
$("trash-close").addEventListener("click", () => $("trash").classList.add("hidden"));
$("trash").addEventListener("click", (e) => { if (e.target === $("trash")) $("trash").classList.add("hidden"); });

// «Фильтр при запуске» показываем только когда включены вкладки SFW/NSFW.
function updateDefaultFilterVis() {
  $("set-default-filter-wrap").classList.toggle("hidden", !$("set-nsfw-split").checked);
}
$("set-nsfw-split").addEventListener("change", updateDefaultFilterVis);

// QR для телефона — показываем, когда галочка включена.
async function updateLanSetup() {
  const on = $("set-lan").checked;
  $("lan-setup").classList.toggle("hidden", !on);
  if (!on) return;
  try {
    const q = await invoke("lan_qr");
    $("lan-qr").innerHTML = q.svg;
    $("lan-url").textContent = "Адрес: " + q.url;
  } catch (e) {
    $("lan-qr").innerHTML = "";
    $("lan-url").textContent = "QR недоступен: " + e;
  }
}
$("set-lan").addEventListener("change", updateLanSetup);

// ---------- Tailscale-доступ (офлайн-галерея на телефоне) ----------
async function refreshTsStatus() {
  const el = $("ts-status");
  el.textContent = "Проверяю Tailscale…";
  let st;
  try { st = await invoke("ts_status"); } catch (e) { el.textContent = "Не удалось проверить: " + e; return; }
  if (!st.installed) {
    el.innerHTML = "Tailscale не найден. Скачай с <b>tailscale.com/download</b>, установи на ПК и на телефон, войди в один аккаунт — и нажми кнопку.";
    $("ts-disable").classList.add("hidden");
    return;
  }
  if (!st.running) {
    el.textContent = "Tailscale установлен, но не в сети. Открой приложение Tailscale и войди в аккаунт.";
    $("ts-disable").classList.add("hidden");
    return;
  }
  el.textContent = st.dns_name ? ("Tailscale в сети: " + st.dns_name) : "Tailscale в сети.";
}

$("ts-enable").addEventListener("click", async () => {
  const btn = $("ts-enable");
  const old = btn.textContent;
  btn.disabled = true; btn.textContent = "Включаю…";
  try {
    const r = await invoke("ts_serve_enable");
    $("ts-qr").innerHTML = r.svg;
    $("ts-url").innerHTML = "Адрес: <b>" + esc(r.url) + "</b>";
    $("ts-setup").classList.remove("hidden");
    $("ts-disable").classList.remove("hidden");
    $("ts-status").textContent = "Доступ включён. Открой адрес на телефоне и добавь на экран «Домой».";
  } catch (e) {
    alert("Tailscale: " + e);
    refreshTsStatus();
  } finally {
    btn.disabled = false; btn.textContent = old;
  }
});

$("ts-disable").addEventListener("click", async () => {
  try {
    await invoke("ts_serve_disable");
    $("ts-setup").classList.add("hidden");
    $("ts-disable").classList.add("hidden");
    $("ts-status").textContent = "Доступ через Tailscale выключен.";
  } catch (e) { alert(e); }
});

// ---------- Telegram-бот-загрузчик ----------
async function refreshTgStatus() {
  let st;
  try { st = await invoke("tg_status"); } catch { st = { enabled: false }; }
  const on = st.enabled && st.has_token;
  $("tg-disconnect").classList.toggle("hidden", !on);
  $("tg-status").textContent = on
    ? "Бот подключён — пересылай ему фото/видео/гиф."
    : "Бот не подключён.";
  if (on) $("tg-token").placeholder = "токен сохранён (вставь новый, чтобы сменить)";
}

$("tg-connect").addEventListener("click", async () => {
  const token = $("tg-token").value.trim();
  if (!token) { alert("Вставь токен бота от @BotFather"); return; }
  const btn = $("tg-connect");
  const old = btn.textContent; btn.disabled = true; btn.textContent = "Проверяю…";
  try {
    const r = await invoke("tg_enable", { token });
    $("tg-token").value = ""; // не держим токен в поле
    $("tg-status").textContent = `Подключён: @${r.username}. Пересылай боту фото/видео/гиф.`;
    $("tg-disconnect").classList.remove("hidden");
  } catch (e) {
    $("tg-status").textContent = "Ошибка: " + e;
  } finally {
    btn.disabled = false; btn.textContent = old;
  }
});

$("tg-disconnect").addEventListener("click", async () => {
  try {
    await invoke("tg_disable");
    $("tg-status").textContent = "Бот отключён.";
    $("tg-disconnect").classList.add("hidden");
  } catch (e) { alert(e); }
});

// Управление коллекциями внутри настроек (порядок / переименование / удаление).
function renderSettingsCollections() {
  const box = $("set-collections");
  if (!box) return;
  if (!state.collections.length) {
    box.innerHTML = `<div class="muted small">Коллекций пока нет. Создай первую на главном экране — «＋ Новая» в панели под поиском.</div>`;
    return;
  }
  // В режиме скрытия NSFW не показываем NSFW-коллекции и здесь (иначе видно имена).
  const cols = state.hideNsfw ? state.collections.filter((c) => !c.is_nsfw) : state.collections;
  const last = cols.length - 1;
  let html = cols.map((c, i) => (
    `<div class="set-col-row" data-id="${c.id}">` +
      `<span class="set-col-name">${esc(c.name)}</span>` +
      `<span class="cnt">${c.count}</span>` +
      `<select class="set-col-rate" data-act="rating" title="Рейтинг коллекции (для режима скрытия NSFW): авто — по содержимому">` +
        `<option value="" ${c.nsfw == null ? "selected" : ""}>авто</option>` +
        `<option value="0" ${c.nsfw === 0 ? "selected" : ""}>SFW</option>` +
        `<option value="1" ${c.nsfw === 1 ? "selected" : ""}>NSFW</option>` +
      `</select>` +
      `<button data-act="up" title="Выше" ${i === 0 ? "disabled" : ""}>↑</button>` +
      `<button data-act="down" title="Ниже" ${i === last ? "disabled" : ""}>↓</button>` +
      `<button data-act="rename" title="Переименовать">✎</button>` +
      `<button data-act="del" title="Удалить (арты останутся)">🗑</button>` +
    `</div>`
  )).join("");
  if (state.hideNsfw && cols.length < state.collections.length) {
    html += `<div class="muted small" style="margin-top:6px;">NSFW-коллекции скрыты режимом. Выключите его выше, чтобы управлять ими.</div>`;
  }
  box.innerHTML = html;
}

// Смена рейтинга коллекции (авто/SFW/NSFW) в списке настроек
$("set-collections").addEventListener("change", async (e) => {
  const sel = e.target.closest("select[data-act='rating']");
  if (!sel) return;
  const id = Number(sel.closest(".set-col-row").dataset.id);
  const val = sel.value === "" ? null : Number(sel.value);
  try {
    await invoke("set_collection_nsfw", { id, nsfw: val });
    await refreshCollections(); // is_nsfw пересчитается
  } catch (err) { alert(err); }
});

async function moveCollection(id, dir) {
  const ids = state.collections.map((c) => c.id);
  const from = ids.indexOf(id);
  const to = from + dir;
  if (from < 0 || to < 0 || to >= ids.length) return;
  ids.splice(to, 0, ids.splice(from, 1)[0]);
  try {
    await invoke("reorder_collections", { ids });
    await refreshCollections();     // перечитает state.collections в новом порядке
    renderSettingsCollections();
  } catch (err) { alert(err); }
}

$("set-collections").addEventListener("click", async (e) => {
  const btn = e.target.closest("button[data-act]");
  if (!btn) return;
  const row = btn.closest(".set-col-row");
  const id = Number(row.dataset.id);
  const col = state.collections.find((c) => c.id === id);
  const act = btn.dataset.act;
  if (act === "up") return moveCollection(id, -1);
  if (act === "down") return moveCollection(id, 1);
  if (act === "del") {
    const yes = await tauriDialog.ask(
      "Удалить коллекцию «" + (col ? col.name : "") + "»? Сами арты останутся в библиотеке.",
      { title: "FurryManager", kind: "warning" });
    if (!yes) return;
    try { await invoke("delete_collection", { id }); } catch (err) { return alert(err); }
    if (state.collection === id) state.collection = null;
    await refreshCollections();
    renderSettingsCollections();
    loadMore(true);
    return;
  }
  if (act === "rename") {
    const nameEl = row.querySelector(".set-col-name");
    const old = col ? col.name : "";
    nameEl.innerHTML = `<input type="text" value="${esc(old)}" />`;
    const input = nameEl.querySelector("input");
    input.focus();
    input.select();
    let done = false;
    const commit = async (save) => {
      if (done) return;
      done = true;
      const v = input.value.trim();
      if (save && v && v !== old) {
        try { await invoke("rename_collection", { id, name: v }); }
        catch (err) { alert(err); }
        await refreshCollections();
      }
      renderSettingsCollections();
    };
    input.addEventListener("keydown", (ev) => {
      if (ev.key === "Enter") commit(true);
      else if (ev.key === "Escape") commit(false);
    });
    input.addEventListener("blur", () => commit(true));
  }
});

// Связи тегов (синонимы / импликации)
async function renderTagRelations() {
  const box = $("rel-list");
  if (!box) return;
  let rels = [];
  try { rels = await invoke("list_tag_relations"); } catch { rels = []; }
  if (!rels.length) {
    box.innerHTML = `<div class="muted small">Связей пока нет.</div>`;
    return;
  }
  box.innerHTML = rels.map((r) =>
    `<div class="rel-row" data-id="${r.id}">` +
      `<span class="rel-tag">${esc(r.from)}</span>` +
      `<span class="rel-op">${r.kind === "alias" ? "=" : "→"}</span>` +
      `<span class="rel-tag">${esc(r.to)}</span>` +
      `<span class="rel-kind muted small">${r.kind === "alias" ? "синоним" : "подразумевает"}</span>` +
      `<button data-act="del" title="Удалить связь">🗑</button>` +
    `</div>`
  ).join("");
}
$("rel-add").addEventListener("click", async () => {
  const from = $("rel-from").value.trim();
  const to = $("rel-to").value.trim();
  const kind = $("rel-kind").value;
  if (!from || !to) { alert("Укажи оба тега"); return; }
  try {
    await invoke("add_tag_relation", { from, to, kind });
  } catch (e) { return alert(e); }
  $("rel-from").value = "";
  $("rel-to").value = "";
  renderTagRelations();
});
$("rel-list").addEventListener("click", async (e) => {
  const btn = e.target.closest("button[data-act='del']");
  if (!btn) return;
  const id = Number(btn.closest(".rel-row").dataset.id);
  try { await invoke("delete_tag_relation", { id }); } catch (err) { return alert(err); }
  renderTagRelations();
});
// Enter во втором поле — быстрый добавить
$("rel-to").addEventListener("keydown", (e) => { if (e.key === "Enter") $("rel-add").click(); });

// Отслеживаемые папки (watcher)
async function renderWatched() {
  const box = $("watch-list");
  if (!box) return;
  let list = [];
  try { list = await invoke("list_watched"); } catch { list = []; }
  if (!list.length) {
    box.innerHTML = `<div class="muted small">Пока нет отслеживаемых папок.</div>`;
    return;
  }
  box.innerHTML = list.map((w) =>
    `<div class="watch-row" data-path="${esc(w.path)}">` +
      `<span class="watch-path" title="${esc(w.path)}">${esc(w.path)}</span>` +
      `<span class="watch-badge muted small">${w.in_place ? "📎 на месте" : "📁 копия"}</span>` +
      `<button data-act="del" title="Перестать следить (арты в библиотеке останутся)">🗑</button>` +
    `</div>`
  ).join("");
}
$("watch-add").addEventListener("click", async () => {
  const folder = await tauriDialog.open({ directory: true, title: "Папка для авто-добавления" });
  if (!folder) return;
  const inPlace = $("watch-inplace").checked;
  try {
    await invoke("add_watched", { path: folder, inPlace });
  } catch (e) { return alert(e); }
  renderWatched();
});
$("watch-list").addEventListener("click", async (e) => {
  const btn = e.target.closest("button[data-act='del']");
  if (!btn) return;
  const path = btn.closest(".watch-row").dataset.path;
  try { await invoke("remove_watched", { path }); } catch (err) { return alert(err); }
  renderWatched();
});

// ---------- Экспорт / импорт библиотеки ----------
listen("export-progress", (ev) => {
  const p = ev.payload;
  $("import-toast").classList.remove("hidden");
  $("import-cancel").classList.add("hidden");
  $("import-bar").style.width = (p.total ? (p.done / p.total) * 100 : 0) + "%";
  $("import-info").textContent = `${p.done} / ${p.total} файлов`;
});

$("btn-export").addEventListener("click", async () => {
  const dest = await tauriDialog.save({
    title: "Куда сохранить архив библиотеки",
    defaultPath: "furrymanager-library.zip",
    filters: [{ name: "ZIP-архив", extensions: ["zip"] }],
  });
  if (!dest) return;
  const toast = $("import-toast");
  toast.classList.remove("hidden");
  toast.querySelector(".toast-title").textContent = "Экспортирую библиотеку…";
  $("import-info").textContent = "Собираю файлы…";
  $("import-bar").style.width = "0%";
  try {
    const n = await invoke("export_library", { dest });
    $("import-bar").style.width = "100%";
    $("import-info").textContent = `Готово! В архиве файлов: ${n}.`;
    setTimeout(() => toast.classList.add("hidden"), 5000);
  } catch (e) {
    $("import-info").textContent = "Экспорт: " + e;
  }
});

$("btn-import-lib").addEventListener("click", async () => {
  const src = await tauriDialog.open({
    title: "Архив библиотеки FurryManager",
    filters: [{ name: "ZIP-архив", extensions: ["zip"] }],
  });
  if (!src) return;
  const yes = await tauriDialog.ask(
    "Импортировать библиотеку из архива?\n\nТекущая база (теги, коллекции, группы) будет ЗАМЕНЕНА базой из архива. Файлы артов докопируются. Действие необратимо.",
    { title: "FurryManager", kind: "warning" });
  if (!yes) return;
  const toast = $("import-toast");
  toast.classList.remove("hidden");
  toast.querySelector(".toast-title").textContent = "Импортирую библиотеку…";
  $("import-info").textContent = "Распаковываю…";
  $("import-bar").style.width = "0%";
  try {
    const n = await invoke("import_library", { src });
    $("import-bar").style.width = "100%";
    $("import-info").textContent = `Готово! Обработано файлов: ${n}.`;
    setTimeout(() => toast.classList.add("hidden"), 5000);
    $("settings").classList.add("hidden");
    // Всё новое: настройки, коллекции, лента
    const setup = await invoke("get_setup_state");
    applyMode(setup.mode || "simple");
    state.nsfwSplit = !!setup.nsfw_split;
    state.lossless = !!setup.lossless;
    state.defaultNsfw = setup.default_nsfw ?? null;
    refreshStats();
    refreshCollections();
    loadMore(true);
  } catch (e) {
    $("import-info").textContent = "Импорт: " + e;
  }
});

// Починка HEIC-фото, сохранённых iOS с неверным расширением
$("btn-repair").addEventListener("click", async () => {
  const btn = $("btn-repair");
  btn.disabled = true;
  btn.textContent = "🔧 Чиню…";
  try {
    const r = await invoke("repair_heic");
    let msg = `Записей без превью: ${r.candidates}.`;
    if (r.fixed) msg += ` Сконвертировано HEIC/AVIF: ${r.fixed}.`;
    if (r.refreshed) msg += ` Восстановлено превью: ${r.refreshed}.`;
    if (r.deleted) msg += ` Удалено обрезанных/пустых: ${r.deleted} (импорт оборвался — добавь эти арты заново).`;
    const rest = Object.entries(r.classes || {})
      .filter(([k]) => !["heic", "avif", "empty", "missing", "jpeg", "png", "gif", "webp"].includes(k));
    if (rest.length) {
      msg += `\nНе тронуты: ` + rest.map(([k, v]) => `${k}: ${v}`).join(", ") + `.`;
    }
    if (!r.candidates) msg = "Записей без превью не нашлось — всё целое.";
    alert(msg);
    if (r.fixed || r.deleted || r.refreshed) { refreshStats(); loadMore(true); }
  } catch (e) {
    alert("Починка: " + e);
  } finally {
    btn.disabled = false;
    btn.textContent = "🔧 Починить битые импорты";
  }
});

$("set-open-help").addEventListener("click", () => {
  $("settings").classList.add("hidden");
  $("help").classList.remove("hidden");
});
$("set-open-guide").addEventListener("click", () => {
  $("settings").classList.add("hidden");
  showGuide();
});
$("ext-token-copy").addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText($("ext-token").value);
    $("ext-token-copy").textContent = "✓";
    setTimeout(() => ($("ext-token-copy").textContent = "📋"), 1500);
  } catch {}
});
// Вкладки настроек
document.querySelectorAll(".set-tab-btn").forEach((btn) =>
  btn.addEventListener("click", () => {
    document.querySelectorAll(".set-tab-btn").forEach((b) => b.classList.toggle("active", b === btn));
    document.querySelectorAll(".set-tab").forEach((t) =>
      t.classList.toggle("active", t.dataset.tab === btn.dataset.tab));
  }));

$("btn-settings").addEventListener("click", openSettings);
$("settings-close").addEventListener("click", () => $("settings").classList.add("hidden"));
$("settings").addEventListener("click", (e) => {
  if (e.target === $("settings")) $("settings").classList.add("hidden");
});
$("settings-save").addEventListener("click", async () => {
  const mode = document.querySelector('input[name="set-mode"]:checked').value;
  const nsfwSplit = $("set-nsfw-split").checked;
  const lossless = $("set-lossless").checked;
  const lan = $("set-lan").checked;
  const lockMode = $("set-lockmode").value;
  const browserCookies = $("set-cookies").value;
  const dfVal = $("set-default-filter").value;
  const defaultNsfw = dfVal === "" ? null : Number(dfVal);
  try {
    const lanChanged = lan !== state.lan;
    await invoke("set_settings", { mode, nsfwSplit, defaultNsfw, lossless, lan, lockMode, browserCookies });
    state.lockMode = lockMode;
    state.browserCookies = browserCookies;
    if (lanChanged) {
      alert("Доступ с телефона " + (lan ? "включён" : "выключен") + ". Перезапусти приложение, чтобы сервер переключился.");
    }
    applyMode(mode);
    state.nsfwSplit = nsfwSplit;
    state.lossless = lossless;
    state.lan = lan;
    state.defaultNsfw = defaultNsfw;
    if (!nsfwSplit) state.nsfw = null;
    renderCollectionsBar();
    $("settings").classList.add("hidden");
    loadMore(true);
  } catch (e) { alert(e); }
});

// ---------- Статистика ----------
async function refreshStats() {
  try {
    const s = await invoke("library_stats");
    let text =
      `${s.files} файлов (${s.images} картинок, ${s.videos} видео) · ` +
      `${fmtBytes(s.total_bytes)} · дубликатов пропущено: ${s.dupes_skipped}`;
    // Счётчик текущего вида — когда включён какой-то фильтр
    if (state.query || state.collection !== null || state.nsfw !== null || state.recent || state.favOnly) {
      try {
        const m = await invoke("count_files", {
          search: state.query, collection: state.collection, nsfw: state.nsfw,
          recent: state.recent, favorite: state.favOnly,
        });
        text += ` · в текущем виде: ${m}`;
      } catch {}
    }
    $("statsbar").textContent = text;
  } catch { $("statsbar").textContent = ""; }
}

// Перезагрузка выдачи с сохранением позиции прокрутки: догружаем столько же
// карточек, сколько было, и возвращаем скролл на место (иначе после удаления
// или группировки улетало в самое начало).
async function reloadKeepScroll() {
  if (state.similarTo) { await loadMore(true); return; } // похожие — одна пачка
  const wrap = $("gallery-wrap");
  if (state.smart && state.query) {
    // Умный поиск: повторяем запрос и возвращаем скролл на место
    const keep = wrap.scrollTop;
    await semanticSearch(state.query);
    wrap.scrollTop = keep;
    return;
  }
  const keep = wrap.scrollTop;
  const target = Math.max(state.cards.length, state.limit);
  state.offset = 0;
  state.reachedEnd = false;
  state.cards = [];
  state.lastIndex = null;
  state.pendingReset = false;
  clearSelection();
  galleryClear();
  refreshStats();
  while (state.cards.length < target && !state.reachedEnd) {
    const before = state.cards.length;
    await loadMore(); // без reset — просто следующая пачка
    if (state.cards.length === before) break; // защита от зацикливания
  }
  wrap.scrollTop = keep;
}

// После изменения состава коллекций: если открыт фильтр по коллекции или
// «Без коллекции», выдача устарела — перезагружаем галерею.
function refreshAfterCollectionChange() {
  refreshCollections();
  if (state.collection !== null) reloadKeepScroll();
}

// ---------- Коллекции ----------
async function refreshCollections() {
  try {
    state.collections = await invoke("list_collections");
  } catch { state.collections = []; }
  renderCollectionsBar();
  if (state.viewerIndex >= 0) renderViewerCollections();
}

// ---------- Тотальное скрытие NSFW ----------
async function applyHideNsfw(on) {
  state.hideNsfw = on;
  if (on) state.nsfw = 0; // залочить фильтр на SFW
  await refreshCollections(); // подтянет is_nsfw и перерисует бар с учётом режима
  if (on) {
    const cur = state.collections.find((c) => c.id === state.collection);
    if (cur && cur.is_nsfw) state.collection = null; // была открыта NSFW-коллекция
    renderCollectionsBar();
  }
  doSearch($("search").value);
}

// want=undefined — переключить; иначе задать. Выключение при включённом ПИН
// требует ввода ПИН (включение — свободно, оно лишь усиливает приватность).
async function toggleHideNsfw(want) {
  const target = (want === undefined) ? !state.hideNsfw : want;
  if (target === state.hideNsfw) return;
  if (state.hideNsfw && !target && state.pinEnabled) {
    const pin = prompt("Введите ПИН, чтобы выключить скрытие NSFW:");
    if (pin === null) { const t = $("set-hide-nsfw"); if (t) t.checked = true; return; }
    let ok = false;
    try { ok = await invoke("pin_verify", { pin }); } catch { ok = false; }
    if (!ok) { alert("Неверный ПИН"); const t = $("set-hide-nsfw"); if (t) t.checked = true; return; }
  }
  try { await invoke("set_hide_nsfw", { on: target }); } catch (e) { return alert(e); }
  await applyHideNsfw(target);
  const t = $("set-hide-nsfw"); if (t) t.checked = target;
}
$("set-hide-nsfw").addEventListener("change", (e) => toggleHideNsfw(e.target.checked));

// Настройки видео→GIF применяются сразу
function applyGifSettings() {
  const auto = $("set-gif-auto").checked;
  const secs = Math.max(1, Math.min(60, Number($("set-gif-secs").value) || 5));
  const fps = Math.max(5, Math.min(30, Number($("set-gif-fps").value) || 15));
  const width = Math.max(120, Math.min(960, Number($("set-gif-width").value) || 480));
  const replace = $("set-gif-replace").checked;
  state.gifAuto = auto; state.gifSecs = secs; state.gifReplace = replace;
  state.gifFps = fps; state.gifWidth = width;
  invoke("set_gif_settings", { auto, secs, replace, fps, width }).catch((e) => alert(e));
}
$("set-gif-auto").addEventListener("change", applyGifSettings);
$("set-gif-secs").addEventListener("change", applyGifSettings);
$("set-gif-fps").addEventListener("change", applyGifSettings);
$("set-gif-width").addEventListener("change", applyGifSettings);
$("set-gif-replace").addEventListener("change", applyGifSettings);

function renderCollectionsBar() {
  const bar = $("collections-bar");
  const hide = state.hideNsfw; // режим тотального скрытия NSFW
  const chips = [];
  // Область коллекций: Все + Без коллекции (рядом).
  chips.push(`<span class="col-chip ${state.collection === null ? "active" : ""}" data-id="">📚 Все</span>`);
  chips.push(`<span class="col-chip ${state.collection === -1 ? "active" : ""}" data-id="-1" title="Арты, которые не состоят ни в одной коллекции">📥 Без коллекции</span>`);
  // Разделитель, затем атрибуты: SFW/NSFW и Недавние.
  chips.push(`<span class="col-sep"></span>`);
  if (state.nsfwSplit) {
    chips.push(`<span class="col-chip ${state.nsfw === 0 ? "active" : ""}" data-nsfw="0" title="Только безопасные арты">🌱 SFW</span>`);
    // В режиме скрытия NSFW-вкладку не показываем — фильтр залочен на SFW.
    if (!hide) chips.push(`<span class="col-chip ${state.nsfw === 1 ? "active" : ""}" data-nsfw="1" title="Только 18+ (по рейтингу ИИ или ручной пометке)">🔞 NSFW</span>`);
  }
  chips.push(`<span class="col-chip ${state.favOnly ? "active" : ""}" data-fav="1" title="Только избранное (★). Пометить: правый клик по арту или клавиша F">★ Избранное</span>`);
  chips.push(`<span class="col-chip ${state.recent ? "active" : ""}" data-recent="1" title="История просмотров: недавно открытые арты (свежие сверху)">🕘 Недавние</span>`);
  // Разделитель перед коллекциями пользователя.
  chips.push(`<span class="col-sep"></span>`);
  for (const c of state.collections) {
    if (hide && c.is_nsfw) continue; // NSFW-коллекции скрыты (вместе с названиями)
    chips.push(
      `<span class="col-chip ${state.collection === c.id ? "active" : ""}" data-id="${c.id}" ` +
      `title="Показать только эту коллекцию. Порядок, переименование и удаление — в ⚙ Настройках.">` +
      `${esc(c.name)} <span class="cnt">${c.count}</span></span>`
    );
  }
  chips.push(`<span class="col-chip" id="col-new" title="Коллекция — как альбом: один арт может быть в нескольких">＋ Новая</span>`);
  bar.innerHTML = chips.join("");
}

// Сортировка галереи. Для «случайно» держим зерно: порядок не меняется при
// удалении/подгрузке (иначе арты «уезжают» и страницы дублируются).
function newRandSeed() { state.randSeed = 1 + Math.floor(Math.random() * 2000000000); }
function updateSortUi() {
  $("sort-wrap").classList.toggle("custom", !!state.sort);
  $("sort-shuffle").classList.toggle("hidden", state.sort !== "random");
}
$("sort-select").addEventListener("change", (e) => {
  state.sort = e.target.value || null;
  if (state.sort === "random") newRandSeed(); // новый выбор — новое перемешивание
  try { localStorage.setItem("fm_sort", state.sort || ""); } catch {}
  updateSortUi();
  loadMore(true);
});
$("sort-shuffle").addEventListener("click", () => {
  newRandSeed();
  loadMore(true);
});

// Индикатор ★ в просмотрщике (и подсветка кнопки)
function updateViewerFav() {
  const btn = $("viewer-fav");
  if (!btn) return;
  const c = viewerCard();
  const on = !!(c && c.favorite);
  btn.textContent = on ? "★" : "☆";
  btn.classList.toggle("on", on);
  btn.title = on ? "Убрать из избранного (F)" : "В избранное (F)";
}

// Пометить/снять ★ у артов
async function setFavorite(ids, on) {
  try {
    await invoke("set_favorite", { fileIds: ids, on });
    ids.forEach((id) => {
      const c = state.cards.find((x) => x.id === id);
      if (c) c.favorite = on;
      // Открытая группа/подборка живёт отдельным списком — обновляем и его
      if (state.g && state.g.files) {
        const g = state.g.files.find((x) => x.id === id);
        if (g) g.favorite = on;
      }
      const el = document.querySelector(`.card[data-id="${id}"]`);
      if (!el) return;
      const cur = el.querySelector(".fav");
      if (on && !cur) el.insertAdjacentHTML("beforeend", `<div class="fav">★</div>`);
      if (!on && cur) cur.remove();
    });
    updateViewerFav();
    // В режиме «только избранное» снятое должно исчезнуть из выдачи
    if (state.favOnly && !on) reloadKeepScroll();
  } catch (e) { alert(e); }
}
$("viewer-fav").addEventListener("click", () => {
  const c = viewerCard();
  if (c) setFavorite([c.id], !c.favorite);
});

// Колесо мыши листает панель коллекций горизонтально (вертикального скролла у неё нет)
$("collections-bar").addEventListener("wheel", (e) => {
  if (!e.deltaY) return;
  e.preventDefault();
  $("collections-bar").scrollLeft += e.deltaY;
}, { passive: false });

$("collections-bar").addEventListener("click", async (e) => {
  if (e.target.closest("#col-new")) {
    const chip = e.target.closest("#col-new");
    chip.outerHTML = `<input id="col-new-input" placeholder="Имя коллекции, Enter" />`;
    const input = $("col-new-input");
    input.focus();
    input.addEventListener("keydown", async (ev) => {
      if (ev.key === "Enter" && input.value.trim()) {
        try { await invoke("create_collection", { name: input.value.trim() }); }
        catch (err) { alert(err); }
        refreshCollections();
      } else if (ev.key === "Escape") {
        renderCollectionsBar();
      }
    });
    input.addEventListener("blur", () => renderCollectionsBar());
    return;
  }
  const chip = e.target.closest(".col-chip");
  if (!chip) return;
  // История просмотров
  if (chip.dataset.recent !== undefined) {
    state.recent = !state.recent;
    renderCollectionsBar();
    doSearch($("search").value);
    return;
  }
  // Только избранное
  if (chip.dataset.fav !== undefined) {
    state.favOnly = !state.favOnly;
    renderCollectionsBar();
    doSearch($("search").value);
    return;
  }
  // Вкладки SFW/NSFW
  if (chip.dataset.nsfw !== undefined) {
    if (state.hideNsfw) { state.nsfw = 0; renderCollectionsBar(); return; } // залочено на SFW
    const v = Number(chip.dataset.nsfw);
    state.nsfw = state.nsfw === v ? null : v; // повторный клик снимает фильтр
    renderCollectionsBar();
    doSearch($("search").value);
    return;
  }
  const id = chip.dataset.id === "" ? null : Number(chip.dataset.id);
  state.collection = id;
  renderCollectionsBar();
  doSearch($("search").value); // умный поиск тоже учтёт выбранную коллекцию
});

// ---------- Галерея ----------
function cardHtml(card, i) {
  const isStack = card.group_count > 1;
  let badge = card.kind === "video" ? "▶ видео" : card.kind === "animation" ? "GIF" : "";
  if (isStack) badge = `🧩 ${card.group_count}`;
  const score = typeof card.score === "number" && card.score >= 0
    ? `<div class="badge badge-score">${Math.round(card.score * 100)}%</div>` : "";
  const img = card.thumb
    ? `<img loading="lazy" src="${mediaSrc(card.thumb)}" alt="" />`
    : card.kind === "image" || card.kind === "animation"
      ? `<img loading="lazy" src="${mediaSrc(card.path)}" alt="" />`
      : `<div class="noimg">🎬</div>`;
  const sel = state.selected.has(card.id) ? " selected" : "";
  const fav = card.favorite ? `<div class="fav">★</div>` : "";

  if (isStack) {
    // Стопка: за обложкой выглядывают верхушки следующих страниц (макс. 2 слоя)
    const l2 = card.stack2 ? `<div class="gs gs1" style="background-image:url('${mediaSrc(card.stack2)}')"></div>` : "";
    const l3 = card.stack3 ? `<div class="gs gs2" style="background-image:url('${mediaSrc(card.stack3)}')"></div>` : "";
    const name = card.group_name
      ? `<div class="gname" title="${esc(card.group_name)}">${esc(card.group_name)}</div>` : "";
    return `<div class="card gstack${sel}" data-i="${i}" data-id="${card.id}">` +
      `${l3}${l2}<div class="gcover">${img}${name}</div>` +
      `${badge ? `<div class="badge">${badge}</div>` : ""}${score}${fav}</div>`;
  }
  return `<div class="card${sel}" data-i="${i}" data-id="${card.id}">${img}${badge ? `<div class="badge">${badge}</div>` : ""}${score}${fav}</div>`;
}

async function loadMore(reset = false) {
  if (state.loading) {
    // Сброс во время загрузки нельзя терять (иначе останется старая выдача,
    // например без учёта NSFW-фильтра) — повторим, когда текущая закончится.
    if (reset) state.pendingReset = true;
    return;
  }
  // Режим «похожие на арт»: одна пачка по релевантности, без прокрутки
  if (state.similarTo) {
    if (!reset) return;
    state.loading = true;
    state.offset = 0;
    state.reachedEnd = true;
    state.lastIndex = null;
    clearSelection();
    galleryClear();
    try {
      const res = await invoke("similar_local_cmd", { fileId: state.similarTo.id, limit: 300 });
      state.cards = res.map((r) => ({ ...r, score: r.similarity / 100 }));
      gallerySet(state.cards.map((c, k) => cardHtml(c, k)).join(""));
      $("statsbar").textContent = `≈ Похожие на арт #${state.similarTo.id} — ${res.length} по релевантности`;
      $("empty-state").classList.toggle("hidden", res.length > 0);
      if (!res.length) {
        $("empty-state").querySelector("h2").textContent = "Похожих не нашлось";
        $("empty-state").querySelector("p").innerHTML = "Сними фильтр «похожие» (✕ на чипе у поиска).";
        $("empty-import").classList.add("hidden");
      }
    } catch (e) {
      console.error(e);
    } finally {
      state.loading = false;
    }
    return;
  }
  if (reset) {
    state.offset = 0;
    state.reachedEnd = false;
    state.cards = [];
    state.lastIndex = null;
    clearSelection();
    galleryClear();
    refreshStats(); // сбрасывает «🧠 …» в обычную статистику
  }
  if (state.reachedEnd) return;
  state.loading = true;
  try {
    const batch = await invoke("query_files", {
      search: state.query, offset: state.offset, limit: state.limit,
      collection: state.collection, nsfw: state.nsfw, recent: state.recent,
      favorite: state.favOnly, sort: state.sort || null, seed: state.randSeed,
    });
    if (batch.length < state.limit) state.reachedEnd = true;
    state.offset += batch.length;
    const startIdx = state.cards.length;
    state.cards.push(...batch);
    galleryAppend(batch.map((c, k) => cardHtml(c, startIdx + k)).join(""));
    queueVideoThumbs(batch);
    const empty = state.cards.length === 0;
    $("empty-state").classList.toggle("hidden", !empty);
    if (empty && (state.query || state.collection !== null)) {
      $("empty-state").querySelector("h2").textContent = "Ничего не нашлось";
      $("empty-state").querySelector("p").innerHTML =
        "Попробуй другие слова, сбрось поиск или выбери «📚 Все».";
      $("empty-import").classList.add("hidden");
    } else if (empty) {
      $("empty-state").querySelector("h2").textContent = "Библиотека пока пуста";
      $("empty-state").querySelector("p").innerHTML =
        "Нажми «Импорт» или просто <b>перетащи файлы/папки в это окно</b> —<br/>я скопирую их в библиотеку, уберу дубликаты и сделаю превью.";
      $("empty-import").classList.remove("hidden");
    }
  } catch (e) {
    console.error(e);
  } finally {
    state.loading = false;
    if (state.pendingReset) {
      state.pendingReset = false;
      loadMore(true);
    }
  }
}

// ---------- Превью для видео (без ffmpeg: кадр снимается прямо в приложении) ----------
const thumbQueue = [];
let thumbBusy = false;

function queueVideoThumbs(batch) {
  for (const c of batch) {
    if (c.kind === "video" && !c.thumb && !c._thumbTried) {
      c._thumbTried = true;
      thumbQueue.push(c);
    }
  }
  processThumbQueue();
}

async function processThumbQueue() {
  if (thumbBusy) return;
  const card = thumbQueue.shift();
  if (!card) return;
  thumbBusy = true;
  try {
    const dataUrl = await captureVideoFrame(mediaSrc(card.path));
    const abs = await invoke("save_video_thumb", {
      fileId: card.id, dataB64: dataUrl.split(",")[1],
    });
    card.thumb = abs;
    const el = document.querySelector(`.card[data-id="${card.id}"] .noimg`);
    if (el) el.outerHTML = `<img loading="lazy" src="${mediaSrc(abs)}" alt="" />`;
  } catch (e) {
    console.warn("video thumb:", card.id, e);
  }
  thumbBusy = false;
  if (thumbQueue.length) setTimeout(processThumbQueue, 50);
}

// Несколько кадров видео (по долям длительности) — для индексации без ffmpeg
function captureVideoFrames(src, fracs) {
  return new Promise((resolve, reject) => {
    const v = document.createElement("video");
    v.muted = true;
    v.preload = "auto";
    v.crossOrigin = "anonymous";
    const out = [];
    let idx = 0;
    const timer = setTimeout(() => fail(new Error("таймаут загрузки видео")), 40000);
    function cleanup() { clearTimeout(timer); v.removeAttribute("src"); v.load(); }
    function fail(e) { cleanup(); reject(e); }
    v.addEventListener("error", () => fail(new Error("видео не декодируется")));
    function seekNext() {
      if (idx >= fracs.length) { cleanup(); resolve(out); return; }
      try { v.currentTime = Math.max(0.1, (v.duration || 1) * fracs[idx]); }
      catch (e) { fail(e); }
    }
    v.addEventListener("loadeddata", seekNext);
    v.addEventListener("seeked", () => {
      try {
        const scale = Math.min(1, 448 / Math.max(v.videoWidth, v.videoHeight, 1));
        const cv = document.createElement("canvas");
        cv.width = Math.max(1, Math.round(v.videoWidth * scale));
        cv.height = Math.max(1, Math.round(v.videoHeight * scale));
        cv.getContext("2d").drawImage(v, 0, 0, cv.width, cv.height);
        out.push(cv.toDataURL("image/jpeg", 0.85).split(",")[1]);
        idx++;
        seekNext();
      } catch (e) { fail(e); }
    });
    v.src = src;
  });
}

// Мультикадровая доиндексация видео силами фронтенда (когда на ПК нет ffmpeg):
// теги по 3 кадрам вместо одного превью — NSFW из середины ролика не теряется.
let vfBusy = false;
async function indexVideoFrames(token) {
  if (vfBusy) return;
  let vids = [];
  try { vids = await invoke("list_videos_for_frames"); } catch { return; }
  if (!vids.length) return;
  vfBusy = true;
  const toast = $("import-toast");
  toast.classList.remove("hidden");
  toast.querySelector(".toast-title").textContent = "Видео: кадры для ИИ…";
  $("import-cancel").classList.add("hidden");
  let done = 0;
  for (const v of vids) {
    if (token && token.cancelled) break;
    try {
      const frames = await captureVideoFrames(mediaSrc(v.path), [0.25, 0.5, 0.75]);
      if (frames.length) {
        await invoke("ai_index_video", { fileId: v.id, framesB64: frames });
      }
    } catch (e) {
      console.warn("video frames:", v.id, e);
    }
    done++;
    $("import-bar").style.width = (done / vids.length) * 100 + "%";
    $("import-info").textContent = `Видео ${done} / ${vids.length}`;
  }
  $("import-info").textContent = `Видео проиндексированы по кадрам: ${done}`;
  setTimeout(() => toast.classList.add("hidden"), 4000);
  vfBusy = false;
  refreshStats();
}

// ---------- Видео → GIF ----------
// Снимаем кадры видео фронтендом (ffmpeg не нужен), кодирование GIF — в Rust.
// Возвращает {tooLong} / {skip} / {frames:[png_b64...], delayMs}.
function captureGif(src, maxSecs, fps, maxDim, onFrame) {
  return new Promise((resolve, reject) => {
    const v = document.createElement("video");
    v.muted = true; v.preload = "auto"; v.crossOrigin = "anonymous";
    const timer = setTimeout(() => fail(new Error("таймаут видео")), 60000);
    function cleanup() { clearTimeout(timer); v.removeAttribute("src"); v.load(); }
    function fail(e) { cleanup(); reject(e); }
    v.addEventListener("error", () => fail(new Error("видео не декодируется")));
    const cv = document.createElement("canvas");
    let times = [], idx = 0, out = [], w = 0, h = 0, delayMs = Math.round(1000 / fps);
    v.addEventListener("loadedmetadata", () => {
      const dur = v.duration || 0;
      if (!dur || !isFinite(dur)) { cleanup(); resolve({ skip: true }); return; }
      if (dur > maxSecs + 0.05) { cleanup(); resolve({ tooLong: true }); return; }
      const n = Math.max(2, Math.min(Math.round(dur * fps), 300)); // кадров
      // Задержка кадра — из реальной длительности, чтобы GIF шёл в темпе видео
      // (для длинных видео кадров упирается в лимит, темп сохраняем).
      delayMs = Math.max(20, Math.round((dur * 1000) / n));
      for (let i = 0; i < n; i++) times.push((dur * i) / n);
      const scale = Math.min(1, maxDim / Math.max(v.videoWidth, v.videoHeight, 1));
      w = Math.max(1, Math.round(v.videoWidth * scale));
      h = Math.max(1, Math.round(v.videoHeight * scale));
      cv.width = w; cv.height = h;
      seekNext();
    });
    function seekNext() {
      if (idx >= times.length) {
        cleanup();
        resolve({ frames: out, delayMs });
        return;
      }
      try { v.currentTime = Math.max(0.001, times[idx]); } catch (e) { fail(e); }
    }
    v.addEventListener("seeked", () => {
      try {
        cv.getContext("2d").drawImage(v, 0, 0, w, h);
        out.push(cv.toDataURL("image/png").split(",")[1]);
        idx++;
        if (onFrame) onFrame(idx, times.length);
        seekNext();
      } catch (e) { fail(e); }
    });
    v.src = src;
  });
}

// Токен отмены текущей конвертации (кнопка «Отменить» в тосте).
let activeGifToken = null;

// Общий проход конвертации набора видео. items: [{id, path}]. maxSecs — порог
// (Infinity = принудительно, без ограничения длины). token.cancelled — стоп.
async function convertGifBatch(items, maxSecs, fps, width, replace, token) {
  token = token || { cancelled: false };
  activeGifToken = token;
  const toast = $("import-toast");
  toast.classList.remove("hidden");
  toast.querySelector(".toast-title").textContent = "Видео → GIF…";
  $("import-bar").style.width = "0%";
  $("import-info").textContent = "Начинаю…";
  $("import-cancel").classList.remove("hidden"); // конвертацию можно отменить
  fps = Math.max(5, Math.min(30, fps || 15));
  const maxDim = Math.max(120, Math.min(960, width || 480));
  let done = 0, made = 0;
  for (const v of items) {
    if (token.cancelled) break;
    $("import-info").textContent = `Видео ${done + 1} / ${items.length}: снимаю кадры…`;
    try {
      const r = await captureGif(mediaSrc(v.path), maxSecs, fps, maxDim, (c, t) => {
        $("import-info").textContent = `Видео ${done + 1} / ${items.length}: кадр ${c}/${t}`;
        $("import-bar").style.width =
          Math.round(((done + c / Math.max(1, t)) / items.length) * 100) + "%";
      });
      if (token.cancelled) break;
      if (r && r.frames && r.frames.length) {
        $("import-info").textContent = `Видео ${done + 1} / ${items.length}: собираю GIF…`;
        await invoke("video_to_gif", { fileId: v.id, delayMs: r.delayMs, framesB64: r.frames });
        made++;
        if (replace) { try { await invoke("delete_files", { fileIds: [v.id], expandGroups: false }); } catch {} }
      } else if (maxSecs !== Infinity) {
        await invoke("mark_gif_done", { fileId: v.id }); // длинное/битое при батче — пропуск
      }
    } catch (e) {
      console.warn("gif:", v.id, e);
      if (maxSecs !== Infinity) await invoke("mark_gif_done", { fileId: v.id }).catch(() => {});
    }
    done++;
    $("import-bar").style.width = Math.round((done / items.length) * 100) + "%";
  }
  $("import-cancel").classList.add("hidden");
  const cancelled = token.cancelled;
  activeGifToken = null;
  $("import-bar").style.width = "100%";
  $("import-info").textContent = cancelled
    ? `Отменено. Успело в GIF: ${made}`
    : `Готово. Конвертировано в GIF: ${made}`;
  setTimeout(() => toast.classList.add("hidden"), 3500);
  return made;
}

// Ставит конвертацию в очередь задач (несколько запусков идут по порядку).
async function runGifConvert(manual) {
  if (!manual) {
    if (TQ.items.some((t) => t.kind === "gif")) return; // авто — не плодим
    // Авто: не создаём задачу (и не мигаем тостом), если конвертировать нечего.
    let pend = [];
    try { pend = await invoke("list_videos_for_gif"); } catch {}
    if (!pend.length) return;
  }
  let redoAll = false;
  if (manual) {
    redoAll = !!(await tauriDialog.ask(
      "Переконвертировать заново и уже обработанные видео (чтобы применить новые настройки качества)?\n\nОК — все заново, Отмена — только новые.",
      { title: "FurryManager" }));
  }
  enqueueTask("Видео → GIF", "gif", async (token) => {
    if (redoAll) { try { await invoke("reset_gif_done"); } catch {} }
    let vids = [];
    try { vids = await invoke("list_videos_for_gif"); } catch {}
    let made = 0;
    if (vids.length && !token.cancelled) {
      made = await convertGifBatch(vids, state.gifSecs || 5, state.gifFps, state.gifWidth, state.gifReplace, token);
      refreshStats();
      loadMore(true);
    }
    // Индексируем: наследовать теги неоткуда → WD14 разметит сам GIF.
    if ((made || manual) && !token.cancelled) backgroundIndex();
  });
}

// Принудительная конвертация конкретных видео (из ПКМ), без порога длины.
function forceGifConvert(items, fps, width, replace) {
  if (!items.length) return;
  enqueueTask(`Видео → GIF (${items.length})`, "gif", async (token) => {
    const made = await convertGifBatch(items, Infinity, fps, width, replace, token);
    refreshStats();
    loadMore(true);
    if (made && !token.cancelled) backgroundIndex();
  });
}

// ---------- Очередь задач (показывается, когда задач больше одной) ----------
const TQ = { items: [], running: false, nextId: 1, open: false, current: null };

// Возвращает промис, который резолвится, когда задача завершилась/снята —
// чтобы вызовы вида `await runIndex()` дожидались конца.
function enqueueTask(label, kind, run) {
  let resolveDone;
  const done = new Promise((res) => { resolveDone = res; });
  const t = { id: TQ.nextId++, label, kind, status: "pending", token: { cancelled: false }, run, resolveDone };
  TQ.items.push(t);
  renderTaskQueue();
  pumpTaskQueue();
  return done;
}
async function pumpTaskQueue() {
  if (TQ.running) return;
  // Снять отменённые pending-задачи (их промисы резолвим).
  TQ.items = TQ.items.filter((t) => {
    if (t.status === "pending" && t.token.cancelled) { t.resolveDone(); return false; }
    return true;
  });
  const next = TQ.items.find((t) => t.status === "pending");
  if (!next) { renderTaskQueue(); return; }
  TQ.running = true;
  TQ.current = next;
  next.status = "running";
  renderTaskQueue();
  try { await next.run(next.token); } catch (e) { console.warn("task:", next.label, e); }
  TQ.items = TQ.items.filter((t) => t.id !== next.id);
  TQ.current = null;
  TQ.running = false;
  next.resolveDone();
  renderTaskQueue();
  pumpTaskQueue();
}
function cancelTask(id) {
  const t = TQ.items.find((x) => x.id === id);
  if (!t) return;
  t.token.cancelled = true; // выполняющаяся задача остановится сама (проверяет токен)
  invoke("ai_cancel").catch(() => {}); // если это индексация — тормозим и бэкенд
  if (t.status === "pending") { TQ.items = TQ.items.filter((x) => x.id !== id); t.resolveDone(); }
  renderTaskQueue();
}
function moveTask(id, dir) {
  const i = TQ.items.findIndex((t) => t.id === id);
  const j = i + dir;
  if (i < 0 || j < 0 || j >= TQ.items.length) return;
  if (TQ.items[i].status === "running" || TQ.items[j].status === "running") return; // текущую не двигаем
  [TQ.items[i], TQ.items[j]] = [TQ.items[j], TQ.items[i]];
  renderTaskQueue();
}
function renderTaskQueue() {
  const wrap = $("taskq");
  const n = TQ.items.length;
  if (n <= 1) { wrap.classList.add("hidden"); TQ.open = false; return; } // «только если больше одной»
  wrap.classList.remove("hidden");
  $("taskq-count").textContent = `Задач в очереди: ${n}`;
  $("taskq-toggle").textContent = TQ.open ? "⌄" : "＾";
  const list = $("taskq-list");
  list.classList.toggle("hidden", !TQ.open);
  list.innerHTML = TQ.items.map((t) => {
    const run = t.status === "running";
    return `<div class="tq-row ${run ? "run" : ""}">` +
      `<span class="tq-label">${run ? "▶ " : ""}${esc(t.label)}</span>` +
      (run ? "" :
        `<button class="ghost tq-btn" data-mv="up" data-id="${t.id}" title="Выше">↑</button>` +
        `<button class="ghost tq-btn" data-mv="down" data-id="${t.id}" title="Ниже">↓</button>`) +
      `<button class="ghost tq-btn" data-cancel="${t.id}" title="Отменить">✕</button>` +
      `</div>`;
  }).join("");
}
$("taskq-toggle").addEventListener("click", () => { TQ.open = !TQ.open; renderTaskQueue(); });
$("taskq-list").addEventListener("click", (e) => {
  const mv = e.target.closest("[data-mv]");
  if (mv) return moveTask(Number(mv.dataset.id), mv.dataset.mv === "up" ? -1 : 1);
  const c = e.target.closest("[data-cancel]");
  if (c) cancelTask(Number(c.dataset.cancel));
});

// Окно параметров конвертации выбранных видео в GIF (из ПКМ).
let gifConvItems = [];
function openGifConv(items) {
  if (!items || !items.length) return;
  gifConvItems = items;
  $("gifconv-count").textContent = items.length === 1 ? "1 видео" : `Выбрано видео: ${items.length}`;
  $("gifconv-fps").value = state.gifFps || 15;
  $("gifconv-width").value = state.gifWidth || 480;
  $("gifconv-replace").checked = !!state.gifReplace;
  $("gifconv").classList.remove("hidden");
}
$("gifconv-close").addEventListener("click", () => $("gifconv").classList.add("hidden"));
$("gifconv").addEventListener("click", (e) => { if (e.target === $("gifconv")) $("gifconv").classList.add("hidden"); });
$("gifconv-go").addEventListener("click", async () => {
  const fps = Math.max(5, Math.min(30, Number($("gifconv-fps").value) || 15));
  const width = Math.max(120, Math.min(960, Number($("gifconv-width").value) || 480));
  const replace = $("gifconv-replace").checked;
  $("gifconv").classList.add("hidden");
  await forceGifConvert(gifConvItems, fps, width, replace);
});

function captureVideoFrame(src) {
  return new Promise((resolve, reject) => {
    const v = document.createElement("video");
    v.muted = true;
    v.preload = "auto";
    v.crossOrigin = "anonymous"; // иначе canvas «испачкается» и toDataURL упадёт
    const timer = setTimeout(() => fail(new Error("таймаут загрузки видео")), 20000);
    function cleanup() { clearTimeout(timer); v.removeAttribute("src"); v.load(); }
    function fail(e) { cleanup(); reject(e); }
    v.addEventListener("error", () => fail(new Error("видео не декодируется")));
    v.addEventListener("loadeddata", () => {
      try { v.currentTime = Math.min(0.5, (v.duration || 1) / 2); } catch (e) { fail(e); }
    });
    v.addEventListener("seeked", () => {
      try {
        const scale = Math.min(1, 400 / Math.max(v.videoWidth, v.videoHeight, 1));
        const cv = document.createElement("canvas");
        cv.width = Math.max(1, Math.round(v.videoWidth * scale));
        cv.height = Math.max(1, Math.round(v.videoHeight * scale));
        cv.getContext("2d").drawImage(v, 0, 0, cv.width, cv.height);
        const url = cv.toDataURL("image/jpeg", 0.82);
        cleanup();
        resolve(url);
      } catch (e) { fail(e); }
    });
    v.src = src;
  });
}

// Клик: обычный — открыть, Ctrl — выбрать, Shift — диапазон
$("gallery").addEventListener("click", (e) => {
  const el = e.target.closest(".card");
  if (!el) return;
  const i = Number(el.dataset.i);
  if (e.ctrlKey || e.metaKey) {
    toggleSelect(i);
    state.lastIndex = i;
  } else if (e.shiftKey && state.lastIndex !== null) {
    const [a, b] = [Math.min(state.lastIndex, i), Math.max(state.lastIndex, i)];
    for (let k = a; k <= b; k++) selectIndex(k, true);
    renderSelectBar();
  } else {
    state.lastIndex = i;
    openViewer(i);
  }
});

// Бесконечная прокрутка (до конца init() не грузим — иначе гонка с фильтрами)
new IntersectionObserver((entries) => {
  if (entries[0].isIntersecting && state.ready) loadMore();
}, { root: $("gallery-wrap"), rootMargin: "600px" }).observe($("scroll-sentinel"));

// ---------- Мультивыбор ----------
function cardEl(i) { return document.querySelector(`.card[data-i="${i}"]`); }

function selectIndex(i, on) {
  const card = state.cards[i];
  if (!card) return;
  if (on) state.selected.add(card.id);
  else state.selected.delete(card.id);
  const el = cardEl(i);
  if (el) el.classList.toggle("selected", on);
}

function toggleSelect(i) {
  const card = state.cards[i];
  if (!card) return;
  selectIndex(i, !state.selected.has(card.id));
  renderSelectBar();
}

function clearSelection() {
  state.selected.clear();
  document.querySelectorAll(".card.selected").forEach((el) => el.classList.remove("selected"));
  renderSelectBar();
}

function renderSelectBar() {
  const n = state.selected.size;
  $("select-bar").classList.toggle("hidden", n === 0);
  if (n === 0) hideSelPanel();
  $("sel-count").textContent = `Выбрано: ${n}`;
  document.querySelectorAll(".nsfw-only").forEach((el) =>
    el.classList.toggle("hidden", !state.nsfwSplit));
  // «Убрать из коллекции» — только когда открыт фильтр конкретной коллекции
  $("sel-uncol").classList.toggle("hidden", !(state.collection > 0));
  // «Объединить группы» — когда выделено ≥2 и ВСЕ выделенные являются стопками
  const sel = [...state.selected].map((id) => state.cards.find((c) => c.id === id)).filter(Boolean);
  const allStacks = sel.length >= 2 && sel.every((c) => c.group_count > 1);
  $("sel-merge").classList.toggle("hidden", !allStacks);
}

function hideSelPanel() { $("sel-panel").classList.add("hidden"); $("sel-panel").innerHTML = ""; }
function showSelPanel(html) { $("sel-panel").innerHTML = html; $("sel-panel").classList.remove("hidden"); }

$("sel-clear").addEventListener("click", clearSelection);
$("sel-sfw").addEventListener("click", () => setNsfwFor([...state.selected], 0));
$("sel-nsfw").addEventListener("click", () => setNsfwFor([...state.selected], 1));
$("sel-auto-nsfw").addEventListener("click", () => setNsfwFor([...state.selected], null));

// Выделить все загруженные арты текущего вида (Ctrl+Shift+A)
function selectAllVisible() {
  if (!state.cards.length) return;
  for (let k = 0; k < state.cards.length; k++) selectIndex(k, true);
  state.lastIndex = state.cards.length - 1;
  renderSelectBar();
}

$("sel-del").addEventListener("click", () => deleteFiles([...state.selected], true)); // стопки — целиком

// Убрать выбранные из открытой коллекции
$("sel-uncol").addEventListener("click", async () => {
  if (!(state.collection > 0) || !state.selected.size) return;
  try {
    await invoke("collection_remove_many", {
      collectionId: state.collection,
      fileIds: [...state.selected],
    });
    refreshAfterCollectionChange(); // выдача фильтра обновится, скролл сохранится
  } catch (e) { alert(e); }
});

$("sel-col").addEventListener("click", () => {
  if (!state.collections.length) {
    showSelPanel(`<div class="menu"><div class="menu-item disabled">Коллекций пока нет — создай в панели под поиском (＋ Новая)</div></div>`);
    return;
  }
  showSelPanel(`<div class="menu">` +
    `<div class="menu-title">Добавить выбранные в:</div>` +
    state.collections.map((c) =>
      `<div class="menu-item" data-col="${c.id}">📚 ${esc(c.name)} <span class="cnt">${c.count}</span></div>`).join("") +
    `</div>`);
  $("sel-panel").querySelectorAll(".menu-item[data-col]").forEach((el) =>
    el.addEventListener("click", async () => {
      try {
        await invoke("collection_add_many", {
          collectionId: Number(el.dataset.col),
          fileIds: [...state.selected],
        });
        hideSelPanel();
        refreshAfterCollectionChange();
      } catch (err) { alert(err); }
    }));
});

$("sel-tag").addEventListener("click", () => {
  showSelPanel(`<div class="menu">` +
    `<div class="menu-title">Тег для всех выбранных:</div>` +
    `<input id="sel-tag-input" placeholder="тег или раса:лис, Enter" autocomplete="off" />` +
    `<div id="sel-tag-suggest"></div></div>`);
  const input = $("sel-tag-input");
  input.focus();
  attachTagSuggest(input, $("sel-tag-suggest"), async (value) => {
    const { ns, name } = parseTag(value);
    if (!name) return;
    try {
      await invoke("add_tag_many", {
        fileIds: [...state.selected], namespace: ns, name,
      });
      hideSelPanel();
    } catch (err) { alert(err); }
  });
});

async function deleteFiles(ids, expandGroups = false) {
  if (!ids.length) return;
  const hasStack = expandGroups &&
    ids.some((id) => { const c = state.cards.find((x) => x.id === id); return c && c.group_count > 1; });
  const yes = await tauriDialog.ask(
    (ids.length === 1
      ? "Удалить этот арт из библиотеки?"
      : `Удалить ${ids.length} артов из библиотеки?`) +
    (hasStack ? "\n\nСреди выбранного есть стопки — они удалятся ЦЕЛИКОМ, со всеми страницами." : "") +
    "\n\nИсчезнут копии из библиотеки (и их теги). " +
    "Исходные файлы, откуда они были импортированы, не трогаются.",
    { title: "FurryManager", kind: "warning" });
  if (!yes) return;
  try {
    await invoke("delete_files", { fileIds: ids, expandGroups });
    if (state.viewerIndex >= 0) closeViewer();
    refreshCollections();
    reloadKeepScroll(); // остаёмся на том же месте ленты
  } catch (e) { alert(e); }
}

// ---------- Контекстное меню (ПКМ) ----------
document.addEventListener("contextmenu", (e) => {
  if (e.target.closest("input, textarea")) return; // в полях ввода — системное
  e.preventDefault();
});

function hideCtxSub() { $("ctx-sub").classList.add("hidden"); }
function hideCtxMenu() {
  $("ctx-menu").classList.add("hidden");
  hideCtxSub();
}

// Подменю (flyout) справа от пункта — для длинных списков коллекций/групп
function showCtxSub(anchorEl, entries) {
  const sub = $("ctx-sub");
  sub.innerHTML = entries.map((en, i) => {
    if (en.title) return `<div class="menu-title">${esc(en.title)}</div>`;
    return `<div class="menu-item ${en.disabled ? "disabled" : ""}" data-e="${i}">${en.label}</div>`;
  }).join("");
  sub.classList.remove("hidden");
  const r = anchorEl.getBoundingClientRect();
  const sr = sub.getBoundingClientRect();
  let left = r.right + 4;
  if (left + sr.width > window.innerWidth - 8) left = r.left - sr.width - 4;
  sub.style.left = Math.max(8, left) + "px";
  sub.style.top = Math.min(r.top, window.innerHeight - sr.height - 8) + "px";
  sub.querySelectorAll(".menu-item[data-e]").forEach((el) => {
    const en = entries[Number(el.dataset.e)];
    if (!en.disabled && en.onClick) {
      el.addEventListener("click", (ev) => {
        ev.stopPropagation();
        hideCtxMenu();
        en.onClick();
      });
    }
  });
}

function showCtxMenu(x, y, entries) {
  const menu = $("ctx-menu");
  hideCtxSub();
  menu.innerHTML = entries.map((en, i) => {
    if (en.sep) return `<div class="menu-sep"></div>`;
    if (en.title) return `<div class="menu-title">${esc(en.title)}</div>`;
    const arrow = en.submenu ? " ▸" : "";
    return `<div class="menu-item ${en.disabled ? "disabled" : ""}" data-e="${i}">${en.label}${arrow}</div>`;
  }).join("");
  menu.classList.remove("hidden");
  const r = menu.getBoundingClientRect();
  menu.style.left = Math.min(x, window.innerWidth - r.width - 8) + "px";
  menu.style.top = Math.min(y, window.innerHeight - r.height - 8) + "px";
  menu.querySelectorAll(".menu-item[data-e]").forEach((el) => {
    const en = entries[Number(el.dataset.e)];
    if (en.disabled) return;
    if (en.submenu) {
      el.addEventListener("click", (ev) => {
        ev.stopPropagation(); // меню не закрываем — открываем flyout рядом
        showCtxSub(el, en.submenu);
      });
    } else if (en.onClick) {
      el.addEventListener("click", () => { hideCtxMenu(); en.onClick(); });
    }
  });
}
document.addEventListener("click", () => hideCtxMenu());
$("gallery-wrap").addEventListener("scroll", hideCtxMenu);

$("gallery").addEventListener("contextmenu", async (e) => {
  const el = e.target.closest(".card");
  if (!el) {
    // ПКМ по пустому месту — предложить вставку арта из буфера обмена.
    e.preventDefault();
    const hasImg = await clipboardHasImage();
    const entries = [{ title: "Буфер обмена" }];
    if (hasImg) {
      entries.push({ label: "📋 Вставить арт из буфера", onClick: () => pasteFromClipboard() });
      const where = currentViewLabel();
      if (where) entries.push({ label: where, disabled: true });
    } else {
      entries.push({ label: "📋 В буфере нет картинки", disabled: true });
    }
    showCtxMenu(e.clientX, e.clientY, entries);
    return;
  }
  e.preventDefault();
  const i = Number(el.dataset.i);
  const card = state.cards[i];
  if (!card) return;

  const multi = state.selected.has(card.id) && state.selected.size > 1;
  const ids = multi ? [...state.selected] : [card.id];
  const galleryGroups = await invoke("list_groups").catch(() => []);

  const entries = [];
  if (multi) entries.push({ title: `Выбрано: ${ids.length}` });
  if (!multi) {
    entries.push({ label: "👁 Открыть", onClick: () => openViewer(i) });
    if (card.kind !== "video") {
      entries.push({ label: "📋 Копировать арт", onClick: () => copyArt(card) });
    }
    entries.push({ label: "📂 Показать в проводнике", onClick: () => revealFile(card.path) });
    if (state.mode === "artist" && card.kind !== "video") {
      entries.push({
        label: "🖼 Открыть поверх окон <span class='cnt'>(референс)</span>",
        onClick: () => openRefWindow(card),
      });
    }
    if (card.group_id && card.group_count > 1) {
      entries.push({ label: `🧩 Редактировать группу (${card.group_count})`, onClick: () => openGroupEditor(card.group_id) });
    }
  }
  // Избранное: для одного арта — по его состоянию, для выбора — пометить все
  {
    const allFav = ids.every((id) => {
      const c = state.cards.find((x) => x.id === id);
      return c && c.favorite;
    });
    entries.push({
      label: allFav
        ? (multi ? `☆ Убрать из избранного (${ids.length})` : "☆ Убрать из избранного")
        : (multi ? `★ В избранное (${ids.length})` : "★ В избранное"),
      onClick: () => setFavorite(ids, !allFav),
    });
  }
  // Конвертация видео в GIF (когда функция включена) — для видео в выборе,
  // без ограничения длины (в отличие от авто-порога).
  if (state.gifAuto) {
    const vidItems = ids
      .map((id) => state.cards.find((c) => c.id === id))
      .filter((c) => c && c.kind === "video")
      .map((c) => ({ id: c.id, path: c.path }));
    if (vidItems.length) {
      entries.push({
        label: `🎞 Конвертировать в GIF${vidItems.length > 1 ? ` (${vidItems.length})` : ""}`,
        onClick: () => openGifConv(vidItems),
      });
    }
  }
  entries.push({ sep: true });
  // В режиме скрытия NSFW не показываем NSFW-коллекции и в меню добавления.
  const visibleCols = state.collections.filter((c) => !(state.hideNsfw && c.is_nsfw));
  if (visibleCols.length) {
    const colEntries = visibleCols.map((c) => ({
      label: `📚 ${esc(c.name)}`,
      onClick: async () => {
        try {
          await invoke("collection_add_many", { collectionId: c.id, fileIds: ids });
          refreshAfterCollectionChange();
        } catch (err) { alert(err); }
      },
    }));
    if (visibleCols.length > 5) {
      // Много коллекций — подменю, а не километровое меню
      entries.push({ label: "📚 Добавить в коллекцию", submenu: colEntries });
    } else {
      entries.push({ title: "Добавить в коллекцию" });
      entries.push(...colEntries);
    }
    entries.push({
      label: multi ? `🪄 Авто-распределить (${ids.length})` : "🪄 Авто-распределить по коллекциям",
      onClick: () => openAutoCollections(ids),
    });
  } else {
    entries.push({ label: "📚 Коллекций нет — создай в панели", disabled: true });
  }
  // Добавление в существующую группу (арты, уже состоящие в группах, бэкенд пропустит)
  if (galleryGroups.length) {
    entries.push({ label: "🧩 Добавить в группу", submenu: groupSubmenu(galleryGroups, ids) });
  }
  if (state.nsfwSplit) {
    entries.push({ sep: true });
    entries.push({ title: "Возрастной рейтинг" });
    entries.push({ label: "🔞 Пометить NSFW", onClick: () => setNsfwFor(ids, 1) });
    entries.push({ label: "🌱 Пометить SFW", onClick: () => setNsfwFor(ids, 0) });
    entries.push({ label: "♻ Авто (по тегам ИИ)", onClick: () => setNsfwFor(ids, null) });
  }
  entries.push({ sep: true });
  entries.push({
    label: multi ? `🗑 Удалить ${ids.length} из библиотеки` : "🗑 Удалить из библиотеки",
    onClick: () => deleteFiles(ids, true), // из галереи стопки удаляются целиком
  });
  showCtxMenu(e.clientX, e.clientY, entries);
});

// Пункты подменю «Добавить в группу» — с миниатюрой обложки
function groupSubmenu(groups, ids) {
  return groups.map((g) => ({
    label:
      (g.cover ? `<img class="mi-thumb" src="${mediaSrc(g.cover)}" alt=""/>` : "") +
      `${g.kind === "comic" ? "📖" : "🧬"} ${esc(g.name || "Без имени")} <span class="cnt">${g.count}</span>`,
    onClick: () => addToGroup(g.id, ids),
  }));
}

// Показать файл в проводнике: ошибки не глотаем, иначе пункт кажется «мёртвым»
async function revealFile(path) {
  try {
    await opener.revealItemInDir(path);
  } catch (e) {
    alert("Не удалось показать файл: " + e);
  }
}

// Добавить арты в существующую группу (в конец; уже сгруппированные пропускаются)
async function addToGroup(groupId, ids) {
  try {
    const added = await invoke("group_add_files", { id: groupId, fileIds: ids });
    if (added < ids.length) {
      alert(`Добавлено: ${added} из ${ids.length} (уже состоящие в группах пропущены).`);
    }
    reloadKeepScroll();
  } catch (e) { alert(e); }
}

// Копирование арта в буфер обмена (буфер принимает только PNG)
async function copyArt(card) {
  try {
    const blob = await (await fetch(mediaSrc(card.path))).blob();
    let png = blob;
    if (blob.type !== "image/png") {
      const bmp = await createImageBitmap(blob); // у GIF возьмётся первый кадр
      const cv = document.createElement("canvas");
      cv.width = bmp.width; cv.height = bmp.height;
      cv.getContext("2d").drawImage(bmp, 0, 0);
      png = await new Promise((res) => cv.toBlob(res, "image/png"));
    }
    await navigator.clipboard.write([new ClipboardItem({ "image/png": png })]);
  } catch (e) {
    alert("Не удалось скопировать: " + e);
  }
}

// Есть ли в буфере обмена картинка (для пункта меню на пустом месте).
async function clipboardHasImage() {
  try {
    if (!navigator.clipboard || !navigator.clipboard.read) return false;
    const items = await navigator.clipboard.read();
    return items.some((it) => it.types.some((t) => t.startsWith("image/")));
  } catch { return false; }
}

// Подпись «куда вставим» — текущая коллекция и метка SFW/NSFW открытого вида.
function currentViewLabel() {
  const parts = [];
  if (state.collection) {
    const c = state.collections.find((x) => x.id === state.collection);
    if (c) parts.push(`в «${c.name}»`);
  }
  if (state.nsfwSplit && state.nsfw !== null) {
    parts.push(state.nsfw === 1 ? "как NSFW" : "как SFW");
  }
  return parts.length ? "→ " + parts.join(", ") : null;
}

function bytesToBase64(bytes) {
  let bin = "";
  const chunk = 0x8000; // btoa не любит слишком длинные строки за раз
  for (let i = 0; i < bytes.length; i += chunk) {
    bin += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk));
  }
  return btoa(bin);
}

// Вставка арта из буфера обмена с присвоением свойств открытого вида.
async function pasteFromClipboard() {
  let blob = null, mime = "image/png";
  try {
    const items = await navigator.clipboard.read();
    for (const it of items) {
      const t = it.types.find((x) => x.startsWith("image/"));
      if (t) { blob = await it.getType(t); mime = t; break; }
    }
  } catch (e) {
    alert("Не удалось прочитать буфер обмена: " + e);
    return;
  }
  if (!blob) { alert("В буфере обмена нет изображения"); return; }

  const toast = $("import-toast");
  toast.classList.remove("hidden");
  toast.querySelector(".toast-title").textContent = "Вставляю из буфера…";
  $("import-cancel").classList.add("hidden");
  $("import-bar").style.width = "30%";
  $("import-info").textContent = "Сохраняю картинку…";

  try {
    const buf = new Uint8Array(await blob.arrayBuffer());
    const res = await invoke("import_clipboard", {
      dataB64: bytesToBase64(buf),
      ext: (mime.split("/")[1] || "png").toLowerCase(),
      // -1 = фильтр «Без коллекции», в него класть нельзя
      collectionId: state.collection > 0 ? state.collection : null,
      nsfw: state.nsfwSplit ? state.nsfw : null,
    });
    $("import-bar").style.width = "100%";
    $("import-info").textContent = res.imported
      ? "Добавлено из буфера ✓"
      : "Уже было в библиотеке (дубликат)";
    setTimeout(() => toast.classList.add("hidden"), 3500);
    refreshStats();
    refreshCollections();
    loadMore(true);
    if (res.imported) backgroundIndex();
  } catch (e) {
    $("import-info").textContent = "Ошибка вставки: " + e;
    setTimeout(() => toast.classList.add("hidden"), 6000);
  }
}

async function setNsfwFor(ids, value) {
  try {
    await invoke("set_nsfw", { fileIds: ids, value });
    reloadKeepScroll();
  } catch (err) { alert(err); }
}

// ---------- Режим «похожие на арт» в галерее ----------
function enterSimilarMode(card) {
  if (!card) return;
  state.similarTo = card;
  if (state.viewerIndex >= 0) closeViewer();
  state.query = "";
  $("search").value = "";
  renderSimilarChip();
  loadMore(true);
}
function exitSimilarMode() {
  state.similarTo = null;
  renderSimilarChip();
  refreshStats();
  loadMore(true);
}
function renderSimilarChip() {
  const chip = $("sim-chip");
  if (!state.similarTo) {
    chip.classList.add("hidden");
    chip.innerHTML = "";
    return;
  }
  const c = state.similarTo;
  chip.innerHTML =
    `<img src="${mediaSrc(c.thumb || c.path)}" alt="" title="Открыть этот арт"/>` +
    `<span class="muted small">похожие</span>` +
    `<span class="x" title="Снять фильтр">✕</span>`;
  chip.classList.remove("hidden");
}
$("sim-chip").addEventListener("click", (e) => {
  if (e.target.classList.contains("x")) { exitSimilarMode(); return; }
  if (state.similarTo) openVirtual([state.similarTo], 0, "Арт-образец");
});

// ---------- Поиск ----------
function doSearch(q) {
  state.query = q.trim();
  if (state.similarTo && state.query) {
    // Пользователь начал обычный поиск — снимаем режим похожих
    state.similarTo = null;
    renderSimilarChip();
  }
  if (state.smart && state.query) {
    semanticSearch(state.query);
    return;
  }
  loadMore(true);
}

// Умный поиск: показывает топ по релевантности, без прокрутки
const STRICT_SCORE = 0.25;

async function semanticSearch(q) {
  state.reachedEnd = true;
  state.offset = 0;
  state.lastIndex = null;
  clearSelection();
  galleryClear();
  $("empty-state").classList.add("hidden");
  $("statsbar").textContent = "🧠 Думаю…";
  try {
    state.smartAll = await invoke("semantic_search", {
      query: q, limit: 100,
      collection: state.collection, nsfw: state.nsfw,
    });
    renderSmartResults();
  } catch (e) {
    refreshStats();
    alert(e);
  }
}

function renderSmartResults() {
  const res = state.strict
    ? state.smartAll.filter((c) => c.score >= STRICT_SCORE || c.score < 0)
    : state.smartAll;
  state.cards = res;
  state.lastIndex = null;
  gallerySet(res.map((c, k) => cardHtml(c, k)).join(""));
  $("statsbar").innerHTML =
    `🧠 «${esc(state.query)}» — ${res.length} по релевантности ` +
    `<button id="strict-toggle" class="${state.strict ? "on" : ""}" ` +
    `title="Скрыть слабые совпадения (ниже ${STRICT_SCORE * 100}%)">только точные</button>`;
  const empty = res.length === 0;
  $("empty-state").classList.toggle("hidden", !empty);
  if (empty) {
    $("empty-state").querySelector("h2").textContent = "Ничего похожего не нашлось";
    $("empty-state").querySelector("p").innerHTML = state.strict && state.smartAll.length
      ? "Точных совпадений нет — выключи «только точные», чтобы увидеть слабые."
      : "Проверь, проиндексирована ли библиотека (кнопка 🧠 справа вверху),<br/>и попробуй описать по-английски: «red fox in jacket».";
    $("empty-import").classList.add("hidden");
  }
}

$("statsbar").addEventListener("click", (e) => {
  if (e.target.id === "strict-toggle") {
    state.strict = !state.strict;
    renderSmartResults();
  }
});

$("smart-toggle").addEventListener("click", () => {
  state.smart = !state.smart;
  $("smart-toggle").classList.toggle("on", state.smart);
  updatePlaceholder();
  const q = $("search").value.trim();
  if (state.smart) {
    $("search").focus();
    if (q) doSearch(q);
  } else {
    refreshStats();
    doSearch(q);
  }
});
$("search").addEventListener("keydown", (e) => {
  if (e.key === "Enter") doSearch(e.target.value);
});
$("search-clear").addEventListener("click", () => {
  $("search").value = "";
  doSearch("");
});

// ---------- Импорт ----------
async function runImport(paths, inPlace = false) {
  if (!paths || !paths.length) return;
  $("import-toast").classList.remove("hidden");
  $("import-toast").querySelector(".toast-title").textContent =
    inPlace ? "Подключаю (без копирования)…" : "Импортирую…";
  $("import-bar").style.width = "0%";
  $("import-info").textContent = "Сканирую…";
  try {
    const s = await invoke("import_paths", { paths, inPlace });
    $("import-info").textContent =
      `Готово! Добавлено: ${s.imported}, дубликатов пропущено: ${s.duplicates}` +
      (s.errors ? `, ошибок: ${s.errors}` : "");
    $("import-bar").style.width = "100%";
    setTimeout(() => $("import-toast").classList.add("hidden"), 4000);
    refreshStats();
    loadMore(true);
    // Тихая фоновая индексация новых файлов (если ИИ настроен)
    if (s.imported > 0) {
      backgroundIndex();
    }
  } catch (e) {
    $("import-info").textContent = "Ошибка: " + e;
  }
}

async function importFolderDialog(inPlace = false) {
  const folder = await tauriDialog.open({
    directory: true,
    title: inPlace ? "Папка (файлы останутся на месте)" : "Выбери папку с артами",
  });
  if (folder) runImport([folder], inPlace);
}

async function importFilesDialog() {
  const files = await tauriDialog.open({
    multiple: true,
    title: "Выбери арты",
    filters: [{ name: "Изображения и видео", extensions: MEDIA_EXTS }],
  });
  if (files) runImport(Array.isArray(files) ? files : [files]);
}

$("btn-import").addEventListener("click", (e) => {
  e.stopPropagation();
  $("import-menu").classList.toggle("hidden");
});
$("mi-folder").addEventListener("click", () => {
  $("import-menu").classList.add("hidden");
  importFolderDialog();
});
$("mi-files").addEventListener("click", () => {
  $("import-menu").classList.add("hidden");
  importFilesDialog();
});
$("mi-folder-inplace").addEventListener("click", () => {
  $("import-menu").classList.add("hidden");
  importFolderDialog(true);
});
$("mi-paste").addEventListener("click", () => {
  $("import-menu").classList.add("hidden");
  pasteFromClipboard(); // сам покажет «в буфере нет изображения», если пусто
});
document.addEventListener("click", (e) => {
  if (!e.target.closest(".menu-wrap")) {
    $("import-menu").classList.add("hidden");
    $("ai-menu").classList.add("hidden");
  }
});
$("empty-import").addEventListener("click", importFolderDialog);

// ---------- ИИ: меню и индексация ----------
$("btn-ai").addEventListener("click", async (e) => {
  e.stopPropagation();
  $("import-menu").classList.add("hidden");
  const menu = $("ai-menu");
  menu.classList.toggle("hidden");
  if (!menu.classList.contains("hidden")) {
    $("ai-status").textContent = "Проверяю…";
    try {
      const n = await invoke("ai_pending");
      $("ai-status").textContent = n > 0
        ? `Не проиндексировано: ${n}`
        : "Всё проиндексировано ✓";
    } catch {
      $("ai-status").textContent = "Библиотека не настроена";
    }
  }
});
$("ai-run").addEventListener("click", () => {
  $("ai-menu").classList.add("hidden");
  runIndex();
});
$("ai-help").addEventListener("click", () => {
  $("ai-menu").classList.add("hidden");
  $("help").classList.remove("hidden");
});

// ---------- Полная переиндексация ----------
$("ai-reindex").addEventListener("click", () => {
  $("ai-menu").classList.add("hidden");
  $("reindex").classList.remove("hidden");
});
$("ai-gif").addEventListener("click", () => {
  $("ai-menu").classList.add("hidden");
  runGifConvert(true);
});
$("reindex-close").addEventListener("click", () => $("reindex").classList.add("hidden"));
$("reindex").addEventListener("click", (e) => {
  if (e.target === $("reindex")) $("reindex").classList.add("hidden");
});
$("reindex-start").addEventListener("click", async () => {
  const optNsfw = $("re-nsfw").checked;
  const optCol = $("re-col").checked;
  $("reindex").classList.add("hidden");
  try {
    await invoke("ai_reindex_reset"); // сброс векторов и автотегов
  } catch (e) { return alert(e); }
  await runIndex(); // прогресс — в тосте, с кнопкой отмены
  if (optNsfw) {
    try { await invoke("nsfw_reset_all"); } catch (e) { alert(e); }
  }
  refreshStats();
  refreshCollections();
  loadMore(true);
  if (optCol) {
    $("autocol-new").checked = true; // создание новых — как просили в опции
    openAutoCollections([]); // пустой список = вся библиотека
  }
});
$("ai-setup").addEventListener("click", () => {
  $("ai-menu").classList.add("hidden");
  runAiSetup();
});

async function runAiSetup() {
  // Собранный сайдкар — Python и pip не нужны, установка бессмысленна (и падала
  // с «program path has no file name», т.к. пути к python нет).
  try {
    const s = await invoke("sidecar_status");
    if (s && s.packaged) {
      alert("ИИ-компоненты уже встроены — Python устанавливать не нужно. Запускаю индексацию.");
      runIndex();
      return;
    }
  } catch {}
  $("import-toast").classList.remove("hidden");
  $("import-toast").querySelector(".toast-title").textContent = "Установка ИИ…";
  $("import-bar").style.width = "0%";
  $("import-info").textContent = "Устанавливаю Python-пакеты (~300 МБ)…";
  try {
    await invoke("ai_setup");
    $("import-bar").style.width = "100%";
    $("import-info").textContent = "ИИ готов! Запускаю индексацию…";
    setTimeout(runIndex, 1200);
  } catch (e) {
    $("import-info").textContent = "Установка ИИ: " + e;
  }
}

// Скачивание из браузера (расширение → HTTP-сервер → gallery-dl)
listen("web-import", (ev) => {
  const p = ev.payload;
  $("import-toast").classList.remove("hidden");
  $("import-cancel").classList.add("hidden"); // отмена — только для индексации
  $("import-toast").querySelector(".toast-title").textContent = "Скачиваю из сети…";
  $("import-info").textContent = p.line || "";
  if (p.stage === "start") $("import-bar").style.width = "30%";
  if (p.stage === "error") {
    $("import-toast").querySelector(".toast-title").textContent = "Скачивание не удалось";
    $("import-bar").style.width = "0%";
    setTimeout(() => $("import-toast").classList.add("hidden"), 12000);
  }
  if (p.stage === "done") {
    $("import-bar").style.width = "100%";
    setTimeout(() => $("import-toast").classList.add("hidden"), 5000);
    refreshStats();
    refreshCollections();
    loadMore(true);
    backgroundIndex();
  }
});

// Watcher добавил файлы из отслеживаемой папки — обновляем галерею.
listen("watch-import", (ev) => {
  const n = ev.payload && ev.payload.added;
  refreshStats();
  refreshCollections();
  loadMore(true);
  if (n) {
    const toast = $("import-toast");
    toast.classList.remove("hidden");
    $("import-cancel").classList.add("hidden");
    toast.querySelector(".toast-title").textContent = "Авто-добавление из папки";
    $("import-bar").style.width = "100%";
    $("import-info").textContent = `Добавлено новых: ${n}`;
    setTimeout(() => toast.classList.add("hidden"), 4000);
    backgroundIndex();
  }
});

listen("ai-setup", (ev) => {
  const p = ev.payload;
  const line = (p.line || "").slice(0, 90);
  if (p.stage === "models") $("import-bar").style.width = "60%";
  $("import-info").textContent =
    (p.stage === "pip" ? "📦 " : p.stage === "models" ? "🧠 " : "") + line;
});

// Фоновая индексация после импорта: обязательно закрываем тост, иначе он
// «висит» на N/N (событие index-progress показывает его, а закрыть было некому).
// Индексация через очередь задач (одна за раз; verbose — подробные сообщения).
function enqueueIndex(verbose) {
  if (TQ.items.some((t) => t.kind === "index")) return Promise.resolve(); // уже в очереди
  return enqueueTask("ИИ-индексация", "index", async (token) => {
    const toast = $("import-toast");
    toast.classList.remove("hidden");
    toast.querySelector(".toast-title").textContent = "Индексирую (ИИ)…";
    $("import-bar").style.width = "0%";
    $("import-cancel").classList.remove("hidden");
    if (verbose) $("import-info").textContent =
      "Первый запуск скачивает модели (~1 ГБ) — это может занять время, прогресс в консоли.";
    try {
      const n = await invoke("ai_index"); // прогресс — через событие index-progress
      if (n) refreshStats();
      $("import-info").textContent = verbose
        ? (n > 0 ? `Готово! Проиндексировано: ${n}.` : "Всё уже проиндексировано ✓")
        : "";
      $("import-bar").style.width = "100%";
      if (!token.cancelled) await indexVideoFrames(token);      // видео — по кадрам
      if (!token.cancelled && state.gifAuto) runGifConvert(false); // затем в GIF (если включено)
    } catch (e) {
      $("import-info").textContent = "ИИ: " + e;
    } finally {
      $("import-cancel").classList.add("hidden");
      setTimeout(() => $("import-toast").classList.add("hidden"), verbose ? 6000 : 2000);
    }
  });
}
function backgroundIndex() { enqueueIndex(false); }
function runIndex() { return enqueueIndex(true); }

$("import-cancel").addEventListener("click", () => {
  if (TQ.current) TQ.current.token.cancelled = true; // текущая задача очереди
  if (activeGifToken) activeGifToken.cancelled = true;
  invoke("ai_cancel").catch(() => {});
  $("import-info").textContent = "Останавливаю после текущего шага…";
});

listen("index-progress", (ev) => {
  const p = ev.payload;
  $("import-toast").classList.remove("hidden");
  $("import-toast").querySelector(".toast-title").textContent = "Индексирую (ИИ)…";
  $("import-cancel").classList.remove("hidden");
  $("import-bar").style.width = (p.total ? (p.done / p.total) * 100 : 0) + "%";
  $("import-info").textContent = `${p.done} / ${p.total}`;
});

listen("import-progress", (ev) => {
  const p = ev.payload;
  $("import-bar").style.width = (p.total ? (p.done / p.total) * 100 : 0) + "%";
  $("import-info").textContent =
    `${p.done} / ${p.total} · добавлено ${p.imported}, дубликатов ${p.duplicates}` +
    (p.current ? ` · ${p.current}` : "");
});

// Drag & drop файлов в окно
listen("tauri://drag-enter", () => $("drop-overlay").classList.remove("hidden"));
listen("tauri://drag-leave", () => $("drop-overlay").classList.add("hidden"));
listen("tauri://drag-drop", (ev) => {
  $("drop-overlay").classList.add("hidden");
  runImport(ev.payload.paths || []);
});

// ---------- Просмотрщик ----------
// Группа (комикс/вариации): если открыта, листаем её страницы, а не ленту.
// state.g = { id, name, kind, files, idx } | null

function viewerCard() {
  return state.g ? state.g.files[state.g.idx] : state.cards[state.viewerIndex];
}

function updateNavButtons() {
  if (state.g) {
    $("nav-prev").disabled = state.g.idx <= 0;
    $("nav-next").disabled = state.g.idx >= state.g.files.length - 1;
  } else {
    $("nav-prev").disabled = state.viewerIndex <= 0;
    $("nav-next").disabled = state.viewerIndex >= state.cards.length - 1;
  }
}

function renderViewerMedia(card) {
  const media = $("viewer-media");
  if (card.kind === "video") {
    media.innerHTML = `<video src="${mediaSrc(card.path)}" controls autoplay loop></video>`;
  } else {
    media.innerHTML = `<img src="${mediaSrc(card.path)}" alt="" />`;
  }
  resetZoom(); // новый арт — сбрасываем зум и пипетку
  updateViewerFav(); // ★ показываем состояние открытого арта
}

async function loadViewerDetails(card) {
  try {
    const d = await invoke("file_details", { id: card.id });
    let title = (d.sources[0] && d.sources[0].title) || `Арт #${d.id}`;
    if (state.g) {
      const gname = state.g.name || (state.g.kind === "comic" ? "Комикс" : "Вариации");
      title = `${gname} — ${state.g.idx + 1}/${state.g.files.length}`;
    }
    $("v-title").textContent = title;
    const dims = d.width ? `${d.width}×${d.height} · ` : "";
    const artist = d.sources[0] && d.sources[0].artist ? `автор: ${d.sources[0].artist} · ` : "";
    let info = `${artist}${dims}${d.ext.toUpperCase()} · ${fmtBytes(d.size)}`;
    if (state.mode !== "simple") info += `\nsha256: ${d.sha256.slice(0, 16)}…`;
    $("v-info").textContent = info;
    renderTags(d.tags);
    state.viewerCollections = d.collections || [];
    renderViewerCollections();
  } catch (e) { console.error(e); }
  // Секция группы в сайдбаре
  const gs = $("v-group-section");
  if (state.g && !state.g.virtual) {
    gs.style.display = "";
    $("v-group-info").textContent =
      `${state.g.kind === "comic" ? "Комикс" : "Вариации"}${state.g.name ? ` «${state.g.name}»` : ""} · ${state.g.files.length} арт(ов)`;
  } else {
    gs.style.display = "none"; // виртуальные подборки (похожие) — без редактора
  }
}

function showGroupPage(idx) {
  if (!state.g) return;
  const card = state.g.files[idx];
  if (!card) return;
  state.g.idx = idx;
  renderViewerMedia(card);
  updateNavButtons();
  loadViewerDetails(card);
}

async function openViewer(i) {
  const card = state.cards[i];
  if (!card) return;
  state.viewerIndex = i;
  state.g = null;
  $("viewer").classList.remove("hidden");
  $("sim-results").innerHTML = "";
  $("sim-status").textContent = "";

  // Стопка: открываем группу и листаем внутри неё
  if (card.group_id && card.group_count > 1) {
    try {
      const g = await invoke("group_get", { id: card.group_id });
      const idx = Math.max(0, g.files.findIndex((f) => f.id === card.id));
      state.g = { id: g.id, name: g.name, kind: g.kind, files: g.files, idx };
      showGroupPage(idx);
      return;
    } catch (e) { console.error(e); } // группа не загрузилась — покажем как одиночный
  }
  renderViewerMedia(card);
  updateNavButtons();
  loadViewerDetails(card);
  invoke("mark_viewed", { fileId: card.id }).catch(() => {}); // история просмотров
}

function closeViewer() {
  $("viewer").classList.add("hidden");
  $("viewer-media").innerHTML = ""; // останавливает видео
  state.viewerIndex = -1;
  state.g = null;
  resetZoom();
}

// ---------- Зум и пипетка ----------
// flip/rot — инструменты художника: зеркало для проверки композиции и поворот.
const zoom = { scale: 1, x: 0, y: 0, panning: false, sx: 0, sy: 0, pipette: false,
               bitmap: null, flip: false, rot: 0 };

function viewerImg() { return $("viewer-media").querySelector("img"); }

function applyZoom() {
  const el = viewerImg();
  if (el) {
    el.style.transform =
      `translate(${zoom.x}px, ${zoom.y}px) scale(${zoom.scale}) ` +
      `rotate(${zoom.rot}deg) scaleX(${zoom.flip ? -1 : 1})`;
  }
  $("viewer-media").style.cursor =
    zoom.pipette ? "crosshair" : zoom.scale !== 1 ? "grab" : "";
}

function resetZoom() {
  zoom.scale = 1; zoom.x = 0; zoom.y = 0;
  zoom.pipette = false; zoom.bitmap = null; zoom.panning = false;
  zoom.flip = false; zoom.rot = 0;
  applyZoom();
}

// Зум к курсору: точка под мышкой остаётся на месте (а не центр картинки).
function zoomAt(clientX, clientY, factor) {
  const img = viewerImg();
  if (!img) return;
  const old = zoom.scale;
  const next = Math.min(12, Math.max(0.2, old * factor));
  if (next === old) return;
  // Центр картинки на экране = нетрансформированный центр + translate
  // (scale вокруг центра его не смещает, transform-origin по умолчанию 50% 50%).
  const r = img.getBoundingClientRect();
  const cx = r.left + r.width / 2;
  const cy = r.top + r.height / 2;
  const k = next / old;
  // Сдвигаем translate так, чтобы точка под курсором осталась под ним.
  zoom.x += (clientX - cx) * (1 - k);
  zoom.y += (clientY - cy) * (1 - k);
  zoom.scale = next;
  applyZoom();
}

$("viewer-media").addEventListener("wheel", (e) => {
  if (!viewerImg()) return; // видео не зумим
  e.preventDefault();
  zoomAt(e.clientX, e.clientY, e.deltaY < 0 ? 1.2 : 1 / 1.2);
}, { passive: false });

// Отключаем нативный drag картинки — он ломал панораму (курсор «запрещено»)
$("viewer-media").addEventListener("dragstart", (e) => e.preventDefault());

$("viewer-media").addEventListener("pointerdown", (e) => {
  if (zoom.pipette && e.button === 0) { pickColor(e); return; }
  if (e.button !== 0 || zoom.scale === 1) return; // панорама и при отдалении тоже
  e.preventDefault();
  zoom.panning = true;
  zoom.sx = e.clientX - zoom.x;
  zoom.sy = e.clientY - zoom.y;
  e.currentTarget.setPointerCapture(e.pointerId);
});
$("viewer-media").addEventListener("pointermove", (e) => {
  if (!zoom.panning) return;
  if (!(e.buttons & 1)) { zoom.panning = false; return; } // ЛКМ должна быть зажата
  zoom.x = e.clientX - zoom.sx;
  zoom.y = e.clientY - zoom.sy;
  applyZoom();
});
$("viewer-media").addEventListener("pointerup", () => { zoom.panning = false; });
$("viewer-media").addEventListener("pointercancel", () => { zoom.panning = false; });
$("viewer-media").addEventListener("lostpointercapture", () => { zoom.panning = false; });
$("viewer-media").addEventListener("dblclick", () => resetZoom());

async function pickColor(e) {
  const img = viewerImg();
  if (!img) return;
  const rect = img.getBoundingClientRect();
  const rx = (e.clientX - rect.left) / rect.width;
  const ry = (e.clientY - rect.top) / rect.height;
  if (rx < 0 || rx > 1 || ry < 0 || ry > 1) return;
  try {
    if (!zoom.bitmap) {
      const blob = await (await fetch(img.src)).blob();
      zoom.bitmap = await createImageBitmap(blob);
    }
    const bm = zoom.bitmap;
    const cv = document.createElement("canvas");
    cv.width = 1; cv.height = 1;
    const ctx = cv.getContext("2d", { willReadFrequently: true });
    ctx.drawImage(bm, Math.min(bm.width - 1, Math.floor(rx * bm.width)),
      Math.min(bm.height - 1, Math.floor(ry * bm.height)), 1, 1, 0, 0, 1, 1);
    const [r, g, b] = ctx.getImageData(0, 0, 1, 1).data;
    const hex = "#" + [r, g, b].map((v) => v.toString(16).padStart(2, "0")).join("");
    let note = "";
    try { await navigator.clipboard.writeText(hex); note = " · скопировано"; } catch {}
    showCtxMenu(e.clientX, e.clientY, [
      { title: "Пипетка" },
      { label: `<span style="display:inline-block;width:14px;height:14px;border-radius:4px;background:${hex};vertical-align:-2px;margin-right:6px;border:1px solid #555"></span><b>${hex}</b>${note}`, onClick: () => {} },
      { label: `RGB ${r}, ${g}, ${b}`, onClick: () => {} },
      { sep: true },
      { label: "Выключить пипетку", onClick: () => { zoom.pipette = false; applyZoom(); } },
    ]);
  } catch (err) {
    console.error(err);
    alert("Не удалось прочитать цвет: " + err);
  }
}

// Сетка построения поверх арта (правило третей) — инструмент художника.
function gridOverlayOn() { return $("viewer-media").classList.contains("grid3"); }
function toggleGridOverlay() { $("viewer-media").classList.toggle("grid3"); }

// Окно поверх других — чтобы держать референс рядом с редактором.
let onTop = false;
async function toggleAlwaysOnTop() {
  try {
    const w = window.__TAURI__.window.getCurrentWindow();
    onTop = !onTop;
    await w.setAlwaysOnTop(onTop);
    $("btn-ontop").classList.toggle("active", onTop);
    $("btn-ontop").title = onTop
      ? "Окно поверх других: включено — кликни, чтобы выключить"
      : "Окно поверх других (удобно рисовать рядом)";
    // Видимый признак режима: рамка вокруг окна + подсказка, как выключить
    document.body.classList.toggle("ontop", onTop);
    $("ontop-hint").classList.toggle("hidden", !onTop);
  } catch (e) {
    onTop = false;
    alert("Не удалось закрепить окно: " + e);
  }
}
$("btn-ontop").addEventListener("click", toggleAlwaysOnTop);
$("ontop-off").addEventListener("click", toggleAlwaysOnTop);

// Отдельное окошко только с артом — поверх других окон. Художнику обычно нужен
// именно реф под рукой, а не весь менеджер поверх редактора.
async function openRefWindow(card) {
  if (!card) return;
  try {
    const { WebviewWindow } = window.__TAURI__.webviewWindow;
    // Одно окно рефа за раз: старое закрываем
    try {
      const prev = await WebviewWindow.getByLabel("ref");
      if (prev) await prev.close();
    } catch {}
    const src = encodeURIComponent(mediaSrc(card.path));
    const name = encodeURIComponent((card.path || "").split(/[\\/]/).pop() || "");
    const path = encodeURIComponent(card.path || "");
    const w = new WebviewWindow("ref", {
      url: `ref.html?src=${src}&name=${name}&path=${path}&id=${card.id}`,
      title: "Референс — поверх других окон",
      width: 460, height: 620,
      alwaysOnTop: true,
      resizable: true,
      decorations: true,
    });
    w.once("tauri://error", (ev) => {
      alert("Не удалось открыть окно референса: " + JSON.stringify(ev && ev.payload));
    });
  } catch (e) {
    alert("Окно референса: " + e);
  }
}

// Из окна референса попросили показать арт в менеджере — открываем и фокусируем
listen("ref-locate", async (ev) => {
  const id = ev.payload && ev.payload.id;
  if (!id) return;
  try {
    const w = window.__TAURI__.window.getCurrentWindow();
    await w.unminimize().catch(() => {});
    await w.setFocus().catch(() => {});
  } catch {}
  try {
    const [brief] = await invoke("files_brief", { ids: [id] });
    if (!brief) { alert("Этот арт уже удалён из библиотеки."); return; }
    const idx = state.cards.findIndex((c) => c.id === id);
    if (idx >= 0) openViewer(idx); // есть в ленте — открываем прямо там
    else openVirtual([brief], 0, "Из окна референса");
  } catch (e) { alert(e); }
});

// ПКМ в просмотрщике: зум, пипетка, удаление
$("viewer-media").addEventListener("contextmenu", (e) => {
  e.preventDefault();
  e.stopPropagation();
  const entries = [];
  if (viewerImg()) {
    entries.push({ label: "🔍 Приблизить <span class='cnt'>(колесо мыши)</span>", onClick: () => { zoom.scale = Math.min(12, zoom.scale * 1.5); applyZoom(); } });
    entries.push({ label: "🔎 Отдалить", onClick: () => { zoom.scale = Math.max(0.2, zoom.scale / 1.5); applyZoom(); } });
    entries.push({ label: "↩ Как было <span class='cnt'>(двойной клик)</span>", onClick: resetZoom });
    // Инструменты художника — только в режиме «Художник»
    if (state.mode === "artist") {
      entries.push({ sep: true });
      entries.push({
        label: zoom.pipette ? "🎨 Выключить пипетку" : "🎨 Пипетка — кликни по цвету",
        onClick: () => { zoom.pipette = !zoom.pipette; applyZoom(); },
      });
      entries.push({
        label: zoom.flip ? "🪞 Убрать зеркало" : "🪞 Зеркало <span class='cnt'>(проверить композицию)</span>",
        onClick: () => { zoom.flip = !zoom.flip; applyZoom(); },
      });
      entries.push({
        label: "⟳ Повернуть на 90°",
        onClick: () => { zoom.rot = (zoom.rot + 90) % 360; applyZoom(); },
      });
      entries.push({
        label: gridOverlayOn() ? "▦ Убрать сетку" : "▦ Сетка построения (трети)",
        onClick: toggleGridOverlay,
      });
      entries.push({
        label: "🖼 Открыть арт поверх окон <span class='cnt'>(отдельным окном)</span>",
        onClick: () => openRefWindow(viewerCard()),
      });
      entries.push({
        label: onTop ? "🔝 Не держать менеджер поверх" : "🔝 Весь менеджер поверх окон",
        onClick: toggleAlwaysOnTop,
      });
    }
    entries.push({ sep: true });
  }
  const card = viewerCard();
  if (card) {
    if (card.kind !== "video") {
      entries.push({ label: "📋 Копировать арт", onClick: () => copyArt(card) });
    }
    entries.push({ label: "📂 Показать в проводнике", onClick: () => revealFile(card.path) });
    entries.push({ sep: true });
    entries.push({ label: "🗑 Удалить из библиотеки", onClick: () => deleteFiles([card.id]) });
  }
  if (entries.length) showCtxMenu(e.clientX, e.clientY, entries);
});
$("viewer-close").addEventListener("click", closeViewer);
function viewerPrev() {
  if (state.g) { if (state.g.idx > 0) showGroupPage(state.g.idx - 1); }
  else if (state.viewerIndex > 0) openViewer(state.viewerIndex - 1);
}
function viewerNext() {
  if (state.g) { if (state.g.idx < state.g.files.length - 1) showGroupPage(state.g.idx + 1); }
  else if (state.viewerIndex < state.cards.length - 1) openViewer(state.viewerIndex + 1);
}
$("nav-prev").addEventListener("click", viewerPrev);
$("nav-next").addEventListener("click", viewerNext);

// ---------- Теги ----------
function renderTags(tags) {
  state.viewerTags = tags; // для «Похожее по тегам»
  $("v-tags").innerHTML = tags.length
    ? tags.map((t) =>
        `<span class="chip" data-id="${t.id}" data-ns="${esc(t.namespace)}" data-name="${esc(t.name)}">` +
        (t.namespace && state.mode !== "simple" ? `<span class="ns">${esc(t.namespace)}:</span>` : "") +
        `${esc(t.name)}<span class="x" title="Убрать тег">✕</span></span>`
      ).join("")
    : `<span class="muted small">Тегов пока нет — добавь первый!</span>`;
}

$("v-tags").addEventListener("click", async (e) => {
  const chip = e.target.closest(".chip");
  if (!chip) return;
  const card = viewerCard();
  if (e.target.classList.contains("x")) {
    await invoke("remove_tag", { fileId: card.id, tagId: Number(chip.dataset.id) });
    const d = await invoke("file_details", { id: card.id });
    renderTags(d.tags);
  } else {
    const q = chip.dataset.ns ? `${chip.dataset.ns}:${chip.dataset.name}` : chip.dataset.name;
    closeViewer();
    $("search").value = q;
    doSearch(q);
  }
});

async function submitViewerTag(value) {
  const card = viewerCard();
  if (!card) return;
  const { ns, name } = parseTag(value);
  if (!name) return;
  try {
    await invoke("add_tag", { fileId: card.id, namespace: ns, name });
    $("tag-input").value = "";
    const d = await invoke("file_details", { id: card.id });
    renderTags(d.tags);
  } catch (e) { alert(e); }
}
$("tag-add-btn").addEventListener("click", () => submitViewerTag($("tag-input").value));

// ---------- Автодополнение тегов ----------
// input — поле ввода, box — контейнер для подсказок, onSubmit(value) — добавление
function attachTagSuggest(input, box, onSubmit) {
  let items = [];
  let active = -1;
  let timer = null;

  function hide() { box.classList.add("hidden"); box.innerHTML = ""; items = []; active = -1; }

  function render() {
    if (!items.length) { hide(); return; }
    box.innerHTML = items.map((t, i) => {
      const full = t.namespace ? `${t.namespace}:${t.name}` : t.name;
      return `<div class="menu-item ${i === active ? "active" : ""}" data-i="${i}" title="${esc(full)}">` +
        (t.namespace ? `<span class="ns">${esc(t.namespace)}:</span>` : "") +
        `${esc(t.name)} <span class="cnt">×${t.count}</span></div>`;
    }).join("");
    box.classList.remove("hidden");
    box.querySelectorAll(".menu-item").forEach((el) =>
      el.addEventListener("mousedown", (e) => {  // mousedown — раньше blur
        e.preventDefault();
        pick(Number(el.dataset.i));
      }));
  }

  function pick(i) {
    const t = items[i];
    if (!t) return;
    const full = t.namespace ? `${t.namespace}:${t.name}` : t.name;
    input.value = "";
    hide();
    onSubmit(full);
  }

  input.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(async () => {
      const q = input.value.trim();
      if (!q) { hide(); return; }
      try {
        const res = await invoke("suggest_tags", { prefix: q });
        if (input.value.trim() !== q) return; // устаревший ответ
        items = res;
        active = -1;
        render();
      } catch { hide(); }
    }, 120);
  });

  input.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown" && items.length) {
      e.preventDefault();
      active = (active + 1) % items.length;
      render();
    } else if (e.key === "ArrowUp" && items.length) {
      e.preventDefault();
      active = (active - 1 + items.length) % items.length;
      render();
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (active >= 0) pick(active);
      else {
        const v = input.value.trim();
        if (v) { input.value = ""; hide(); onSubmit(v); }
      }
    } else if (e.key === "Escape") {
      hide();
    }
  });

  input.addEventListener("blur", () => setTimeout(hide, 150));
}

attachTagSuggest($("tag-input"), $("tag-suggest"), submitViewerTag);

// ---------- Коллекции в просмотрщике ----------
function renderViewerCollections() {
  const el = $("v-collections");
  if (!state.collections.length) {
    el.innerHTML = `<span class="muted small">Коллекций пока нет — создай первую в панели под поиском (＋ Новая).</span>`;
    return;
  }
  el.innerHTML = state.collections
    .filter((c) => !(state.hideNsfw && c.is_nsfw)) // в режиме скрытия — без NSFW-коллекций
    .map((c) => {
      const inCol = state.viewerCollections.includes(c.id);
      const hint = inCol ? "Убрать из коллекции" : "Добавить в коллекцию";
      const mark = inCol ? "✓ " : "";
      return `<span class="chip ${inCol ? "incol" : ""}" data-col="${c.id}" title="${hint}">${mark}${esc(c.name)}</span>`;
    }).join("");
}

$("v-collections").addEventListener("click", async (e) => {
  const chip = e.target.closest(".chip[data-col]");
  if (!chip) return;
  const card = viewerCard();
  if (!card) return;
  const colId = Number(chip.dataset.col);
  try {
    const added = await invoke("toggle_collection", { collectionId: colId, fileId: card.id });
    if (added) state.viewerCollections.push(colId);
    else state.viewerCollections = state.viewerCollections.filter((id) => id !== colId);
    renderViewerCollections();
    const col = state.collections.find((c) => c.id === colId);
    if (col) col.count += added ? 1 : -1;
    renderCollectionsBar();
  } catch (err) { alert(err); }
});

// ---------- Удаление из просмотрщика ----------
$("btn-delete").addEventListener("click", () => {
  const card = viewerCard();
  if (card) deleteFiles([card.id]);
});

// ---------- Похожие ----------
$("btn-sim-local").addEventListener("click", async () => {
  const card = viewerCard();
  if (!card) return;
  $("sim-status").textContent = "Ищу в библиотеке…";
  $("sim-results").innerHTML = "";
  state.simFor = card; // по кому искали (для «Показать все»)
  try {
    const res = await invoke("similar_local_cmd", { fileId: card.id, limit: 24 });
    state.simCards = res;
    $("sim-status").textContent = res.length
      ? `Найдено: ${res.length}. Клик — открыть, ПКМ — действия.` : "Похожих в библиотеке не нашлось 🤷";
    $("sim-results").innerHTML = res.map((r, k) => {
      const src = r.thumb ? mediaSrc(r.thumb) : mediaSrc(r.path);
      return `<div class="sim-item" data-id="${r.id}" data-k="${k}">
        <img src="${src}" alt="" />
        <div class="sim-meta"><div class="t">схожесть ${r.similarity}%</div>
        <div class="u">${r.width}×${r.height} · ${esc(r.ext)}</div></div></div>`;
    }).join("") + (res.length
      ? `<button id="sim-all" class="secondary small-btn" style="width:100%;margin-top:8px;">Показать все похожие в галерее</button>`
      : "");
    const allBtn = $("sim-all");
    if (allBtn) allBtn.addEventListener("click", () => enterSimilarMode(state.simFor));
  } catch (e) {
    $("sim-status").textContent = e;
  }
});

// Открытие произвольного набора карточек в просмотрщике (вне ленты)
function openVirtual(files, idx, name) {
  if (!files || !files.length) return;
  if (state.viewerIndex < 0) state.viewerIndex = 0; // просмотрщик «открыт»
  $("viewer").classList.remove("hidden");
  state.g = { id: null, virtual: true, name: name || "Подборка", kind: "similar", files, idx };
  showGroupPage(idx);
}

$("sim-results").addEventListener("click", (e) => {
  const item = e.target.closest(".sim-item");
  if (!item) return;
  if (item.dataset.url) { opener.openUrl(item.dataset.url); return; }
  // Локальные похожие: открываем прямо из результатов, даже если их нет в ленте
  const k = Number(item.dataset.k);
  if (state.simCards && state.simCards[k]) {
    openVirtual(state.simCards, k, "Похожие");
  }
});

// ПКМ по локальному похожему: копировать/проводник/в группу/удалить
$("sim-results").addEventListener("contextmenu", async (e) => {
  const item = e.target.closest(".sim-item");
  if (!item || item.dataset.url) return; // веб-результаты не трогаем
  e.preventDefault();
  const id = Number(item.dataset.id);
  const card = (state.simCards || []).find((c) => c.id === id);
  if (!card) return;
  const entries = [];
  if (card.kind !== "video") {
    entries.push({ label: "📋 Копировать арт", onClick: () => copyArt(card) });
  }
  entries.push({ label: "📂 Показать в проводнике", onClick: () => opener.revealItemInDir(card.path) });
  const groups = await invoke("list_groups").catch(() => []);
  const sub = groupSubmenu(groups, [id]);
  // Быстрое создание группы: открытый арт + этот похожий
  const base = state.simFor || viewerCard();
  if (base && base.id !== id) {
    sub.unshift({
      label: `＋ Новая группа с артом #${base.id}`,
      onClick: async () => {
        try {
          const gid = await invoke("group_create", {
            fileIds: [base.id, id],
            kind: "variants",
            name: null,
          });
          openGroupEditor(gid);
          reloadKeepScroll();
        } catch (err) { alert(err); }
      },
    });
  }
  if (sub.length) {
    entries.push({ label: "🧩 Добавить в группу", submenu: sub });
  }
  entries.push({ sep: true });
  entries.push({ label: "🗑 Удалить из библиотеки", onClick: () => deleteFiles([id]) });
  showCtxMenu(e.clientX, e.clientY, entries);
});

// Вид файла по расширению (там, где под рукой нет kind из базы).
function kindFromExt(ext) {
  const e = String(ext || "").toLowerCase().replace(/^\./, "");
  if (["mp4", "webm", "mov", "mkv", "avi", "m4v"].includes(e)) return "video";
  if (e === "gif") return "animation";
  return "image";
}

// Стандартное контекст-меню для арта по объекту {id, path, kind}: открыть,
// копировать, показать в проводнике, удалить. Для модалок, где карточки из
// ленты нет (Дубликаты, Предложить группы). afterDelete — что перерисовать.
function showItemCtx(x, y, item, afterDelete) {
  const entries = [{
    label: "👁 Открыть",
    onClick: () => {
      // Просмотрщик (z-50) ниже модалок (z-70) — прячем модалку, иначе не видно.
      $("dupes").classList.add("hidden");
      $("autogroups").classList.add("hidden");
      openVirtual([item], 0, "Просмотр");
    },
  }];
  if (item.kind !== "video") {
    entries.push({ label: "📋 Копировать арт", onClick: () => copyArt(item) });
  }
  entries.push({ label: "📂 Показать в проводнике", onClick: () => revealFile(item.path) });
  entries.push({ sep: true });
  entries.push({
    label: "🗑 Удалить из библиотеки",
    onClick: async () => { await deleteFiles([item.id]); if (afterDelete) afterDelete(); },
  });
  showCtxMenu(x, y, entries);
}

// ПКМ по арту в модалке «Дубликаты» (все данные уже под рукой в dupeGroups)
$("dupes-list").addEventListener("contextmenu", (e) => {
  const el = e.target.closest(".dupe-item[data-id]");
  if (!el) return;
  e.preventDefault();
  const id = Number(el.dataset.id);
  let file = null;
  for (const g of dupeGroups) {
    const f = g.files.find((x) => x.id === id);
    if (f) { file = f; break; }
  }
  if (!file) return;
  const item = { id, path: file.path, thumb: file.thumb, kind: kindFromExt(file.ext) };
  showItemCtx(e.clientX, e.clientY, item, () => scanDupes());
});

// ПКМ по миниатюре в «Предложить группы» — путь/вид тянем с бэкенда по id
$("ag-list").addEventListener("contextmenu", async (e) => {
  const img = e.target.closest("img.ag-th[data-fid]");
  if (!img) return;
  e.preventDefault();
  const fid = Number(img.dataset.fid);
  let brief;
  try {
    brief = (await invoke("files_brief", { ids: [fid] }))[0];
  } catch (err) { return alert(err); }
  if (!brief) return;
  // После удаления пересобираем предложения (арт исчез из библиотеки)
  showItemCtx(e.clientX, e.clientY, brief, () => $("dupe-autogroup").click());
});

$("btn-sim-web").addEventListener("click", async () => {
  const card = viewerCard();
  if (!card) return;
  $("sim-status").textContent = "Спрашиваю Fluffle.xyz… (нужен интернет)";
  $("sim-results").innerHTML = "";
  try {
    const res = await invoke("similar_web", { fileId: card.id });
    const good = res.filter((r) => r.match_kind !== "unlikely");
    const shown = good.length ? good : res.slice(0, 5);
    $("sim-status").textContent = shown.length
      ? `Найдено: ${shown.length}. Клик — открыть в браузере.`
      : "В интернете совпадений не нашлось.";
    const matchLabel = { exact: "точное", alternative: "вариант", tossUp: "возможно", unlikely: "вряд ли" };
    $("sim-results").innerHTML = shown.map((r) =>
      `<div class="sim-item" data-url="${esc(r.url)}">
        ${r.thumb ? `<img src="${esc(r.thumb)}" alt="" />` : ""}
        <div class="sim-meta">
          <div class="t">${esc(r.platform)} · <span class="match-${esc(r.match_kind)}">${matchLabel[r.match_kind] || esc(r.match_kind)}</span> ${(r.score * 100) | 0}%</div>
          ${r.credits ? `<div>автор: ${esc(r.credits)}</div>` : ""}
          <div class="u">${esc(r.url)}</div>
        </div></div>`
    ).join("");
  } catch (e) {
    $("sim-status").textContent = e;
  }
});

// «Похожее по тегам»: открыть на e621/danbooru/FA поиск по выбранным тегам арта.
function buildTagSearchUrl(site, names) {
  if (site === "furaffinity") {
    return "https://www.furaffinity.net/search/?q=" + encodeURIComponent(names.join(" "));
  }
  // booru-теги: нижний регистр, пробелы → подчёркивания
  const booru = names.map((n) => n.trim().toLowerCase().replace(/\s+/g, "_")).join(" ");
  const base = site === "danbooru"
    ? "https://danbooru.donmai.us/posts"
    : "https://e621.net/posts";
  return base + "?tags=" + encodeURIComponent(booru);
}

$("btn-sim-tags").addEventListener("click", () => {
  const tags = (state.viewerTags || []).filter((t) => t.namespace !== "rating");
  if (!tags.length) {
    $("sim-results").innerHTML = "";
    $("sim-status").textContent =
      "У этого арта нет тегов. Проиндексируй (🧠) для автотегов или добавь теги вручную.";
    return;
  }
  $("sim-status").textContent = "Выбери теги и сайт — открою поиск в браузере.";
  // Предвыбор: персонажи + до 3 общих тегов (без неймспейса), серии/рейтинг пропускаем.
  const selected = new Set();
  let gen = 0;
  for (const t of tags) {
    if (t.namespace === "character") selected.add(t.name);
    else if (!t.namespace && gen < 3) { selected.add(t.name); gen++; }
  }
  const names = tags.map((t) => t.name);
  const chips = tags.map((t, i) =>
    `<span class="ts-chip ${selected.has(t.name) ? "on" : ""}" data-i="${i}">${esc(t.name)}</span>`
  ).join("");
  $("sim-results").innerHTML =
    `<div class="tagsearch">
      <div class="ts-chips">${chips}</div>
      <div class="ts-row">
        <select id="ts-site">
          <option value="e621">e621</option>
          <option value="danbooru">danbooru</option>
          <option value="furaffinity">FurAffinity</option>
        </select>
        <button class="primary small-btn" id="ts-open">Открыть поиск ↗</button>
      </div>
      <div class="ts-note muted small">Автотеги совпадают со словарём e621/danbooru. На FurAffinity поиск по словам — как автор подписал арт. Меньше тегов (2–4) — больше результатов.</div>
    </div>`;
  $("sim-results").querySelectorAll(".ts-chip").forEach((el) => {
    el.addEventListener("click", () => {
      const nm = names[Number(el.dataset.i)];
      if (selected.has(nm)) { selected.delete(nm); el.classList.remove("on"); }
      else { selected.add(nm); el.classList.add("on"); }
    });
  });
  $("ts-open").addEventListener("click", () => {
    const chosen = [...selected];
    if (!chosen.length) { alert("Выбери хотя бы один тег"); return; }
    opener.openUrl(buildTagSearchUrl($("ts-site").value, chosen));
  });
});

// ---------- Горячие клавиши ----------
document.addEventListener("keydown", (e) => {
  if (uiLocked) return; // под шторкой хоткеи не работают
  if (e.target.tagName === "INPUT") {
    if (e.key === "Escape") e.target.blur();
    return;
  }
  if (e.key === "Escape") {
    if (!$("ctx-menu").classList.contains("hidden")) hideCtxMenu();
    else if (!$("trash").classList.contains("hidden")) $("trash").classList.add("hidden");
    else if (!$("gifconv").classList.contains("hidden")) $("gifconv").classList.add("hidden");
    else if (!$("health").classList.contains("hidden")) $("health").classList.add("hidden");
    else if (!$("groupedit").classList.contains("hidden")) $("groupedit").classList.add("hidden");
    else if (!$("autogroups").classList.contains("hidden")) $("autogroups").classList.add("hidden");
    else if (!$("reindex").classList.contains("hidden")) $("reindex").classList.add("hidden");
    else if (!$("autocol").classList.contains("hidden")) $("autocol").classList.add("hidden");
    else if (!$("dupes").classList.contains("hidden")) $("dupes").classList.add("hidden");
    else if (!$("help").classList.contains("hidden")) $("help").classList.add("hidden");
    else if (!$("settings").classList.contains("hidden")) $("settings").classList.add("hidden");
    else if (!$("viewer").classList.contains("hidden")) closeViewer();
    else if (state.selected.size) clearSelection();
  } else if ((e.ctrlKey || e.metaKey) && !e.shiftKey && (e.key === "L" || e.key === "l" || e.key === "Д" || e.key === "д")) {
    e.preventDefault();
    lockNow(); // мгновенная шторка (Ctrl+L)
  } else if ((e.ctrlKey || e.metaKey) && e.shiftKey && (e.key === "A" || e.key === "a" || e.key === "Ф" || e.key === "ф")) {
    e.preventDefault();
    selectAllVisible(); // работает и на русской раскладке
  } else if ((e.ctrlKey || e.metaKey) && e.shiftKey && (e.code === "KeyH")) {
    e.preventDefault();
    toggleHideNsfw(); // тотальное скрытие NSFW (Ctrl+Shift+H, любая раскладка)
  } else if (!e.ctrlKey && !e.metaKey && !e.altKey && e.code === "KeyF") {
    // F — переключить ★ у открытого арта или у выделенных
    const ids = state.selected.size ? [...state.selected]
      : (viewerCard() ? [viewerCard().id] : []);
    if (ids.length) {
      e.preventDefault();
      const allFav = ids.every((id) => {
        const c = state.cards.find((x) => x.id === id);
        return c && c.favorite;
      });
      setFavorite(ids, !allFav);
    }
  } else if (e.key === "/") {
    e.preventDefault();
    $("search").focus();
  } else if (state.viewerIndex >= 0 && e.key === "ArrowRight") {
    viewerNext();
  } else if (state.viewerIndex >= 0 && e.key === "ArrowLeft") {
    viewerPrev();
  } else if (e.key === "Delete") {
    if (state.viewerIndex >= 0) {
      const card = viewerCard();
      if (card) deleteFiles([card.id]);
    } else if (state.selected.size) {
      deleteFiles([...state.selected], true); // из галереи — стопки целиком
    }
  }
});

// ---------- Справка ----------
$("btn-help").addEventListener("click", () => $("help").classList.remove("hidden"));
$("help-close").addEventListener("click", () => $("help").classList.add("hidden"));
// клик по тёмному фону тоже закрывает
$("help").addEventListener("click", (e) => {
  if (e.target === $("help")) $("help").classList.add("hidden");
});

// ---------- Менеджер тегов (массовое редактирование) ----------
let tmTags = [];
const tmSel = new Set();

async function renderTagMgr() {
  const q = $("tm-search").value.trim();
  try { tmTags = await invoke("list_all_tags", { search: q }); }
  catch (e) { $("tm-status").textContent = "Ошибка: " + e; return; }
  $("tm-status").textContent = `Показано: ${tmTags.length}${tmTags.length >= 500 ? " (первые 500 — уточни поиск)" : ""}`;
  $("tm-list").innerHTML = tmTags.map((t, i) => {
    const full = t.namespace ? `${t.namespace}:${t.name}` : t.name;
    const auto = t.count - t.manual;
    return `<div class="tm-row" data-i="${i}">
      <input type="checkbox" data-i="${i}" ${tmSel.has(t.id) ? "checked" : ""}/>
      <span class="tm-name" title="${esc(full)}">${t.namespace ? `<span class="ns">${esc(t.namespace)}:</span>` : ""}${esc(t.name)}</span>
      <span class="muted small tm-cnt">${t.count} шт${auto > 0 ? ` · авто ${auto}` : ""}</span>
      <button data-act="find" title="Показать эти арты в галерее">🔍</button>
      <button data-act="ren" title="Переименовать во всей библиотеке">✎</button>
    </div>`;
  }).join("") || `<div class="muted small">Ничего не найдено</div>`;
  updateTmButtons();
}

function updateTmButtons() {
  const n = tmSel.size;
  $("tm-sel").textContent = n ? `Выбрано тегов: ${n}` : "";
  $("tm-merge").disabled = n < 2;
  $("tm-delete").disabled = n === 0;
}

$("tm-list").addEventListener("change", (e) => {
  const cb = e.target.closest("input[type=checkbox][data-i]");
  if (!cb) return;
  const t = tmTags[Number(cb.dataset.i)];
  if (!t) return;
  if (cb.checked) tmSel.add(t.id); else tmSel.delete(t.id);
  updateTmButtons();
});

$("tm-list").addEventListener("click", async (e) => {
  const btn = e.target.closest("button[data-act]");
  if (!btn) return;
  const t = tmTags[Number(btn.closest(".tm-row").dataset.i)];
  if (!t) return;
  const full = t.namespace ? `${t.namespace}:${t.name}` : t.name;
  if (btn.dataset.act === "find") {
    $("tagmgr").classList.add("hidden");
    $("settings").classList.add("hidden");
    $("search").value = full;
    doSearch(full);
    return;
  }
  if (btn.dataset.act === "ren") {
    const val = prompt("Новое имя тега (можно с неймспейсом, напр. раса:лис):", full);
    if (!val || val.trim() === full) return;
    const { ns, name } = parseTag(val.trim());
    try {
      await invoke("tag_rename", { id: t.id, namespace: ns, name });
      tmSel.clear();
      renderTagMgr();
      loadMore(true);
    } catch (err) { alert(err); }
  }
});

$("tm-merge").addEventListener("click", async () => {
  const ids = [...tmSel];
  if (ids.length < 2) return;
  const chosen = tmTags.filter((t) => ids.includes(t.id));
  const names = chosen.map((t) => (t.namespace ? `${t.namespace}:${t.name}` : t.name));
  const keep = prompt(
    "В какой тег слить? Введи номер:\n\n" + names.map((n, i) => `${i + 1}. ${n}`).join("\n"),
    "1");
  const idx = Number(keep) - 1;
  if (!(idx >= 0 && idx < chosen.length)) return;
  const target = chosen[idx].id;
  try {
    await invoke("tags_merge", { ids: ids.filter((x) => x !== target), target });
    tmSel.clear();
    renderTagMgr();
    loadMore(true);
  } catch (e) { alert(e); }
});

$("tm-delete").addEventListener("click", async () => {
  const ids = [...tmSel];
  if (!ids.length) return;
  const yes = await tauriDialog.ask(
    `Удалить ${ids.length} тег(ов) из всей библиотеки? Сами арты останутся, снимутся только эти метки.`,
    { title: "FurryManager", kind: "warning" });
  if (!yes) return;
  try {
    const n = await invoke("tags_delete", { ids });
    alert(`Готово. Снято меток: ${n}.`);
    tmSel.clear();
    renderTagMgr();
    loadMore(true);
  } catch (e) { alert(e); }
});

let tmTimer = null;
$("tm-search").addEventListener("input", () => {
  clearTimeout(tmTimer);
  tmTimer = setTimeout(renderTagMgr, 250);
});
// Смена папки библиотеки (например, перенесли на другой диск)
async function chooseLibraryFolder(hint) {
  const folder = await tauriDialog.open({ directory: true, title: hint || "Папка библиотеки FurryManager" });
  if (!folder) return false;
  try {
    await invoke("setup_library", { path: folder, mode: state.mode });
    state.libraryRoot = folder;
    $("lib-path").textContent = folder;
    refreshStats();
    refreshCollections();
    loadMore(true);
    return true;
  } catch (e) { alert(e); return false; }
}
$("btn-lib-change").addEventListener("click", async () => {
  const yes = await tauriDialog.ask(
    "Выбрать другую папку библиотеки?\n\nЭто переключит приложение на библиотеку в выбранной папке. Файлы никуда не переносятся: текущая библиотека остаётся на месте, к ней можно вернуться тем же способом.",
    { title: "FurryManager", kind: "warning" });
  if (!yes) return;
  if (await chooseLibraryFolder()) $("settings").classList.add("hidden");
});
$("btn-lib-open").addEventListener("click", async () => {
  if (!state.libraryRoot) { alert("Библиотека ещё не выбрана"); return; }
  try {
    await opener.openPath(state.libraryRoot);
  } catch (e) {
    // Не молчим: раньше ошибка гасилась и кнопка выглядела «мёртвой»
    try { await opener.revealItemInDir(state.libraryRoot); }
    catch (e2) { alert("Не удалось открыть папку: " + (e2 || e)); }
  }
});

$("btn-ai-install").addEventListener("click", () => {
  $("settings").classList.add("hidden");
  runAiSetup(); // pip install + прогрев моделей, прогресс в тосте
});

$("btn-tagmgr").addEventListener("click", () => {
  tmSel.clear();
  $("tm-search").value = "";
  $("tagmgr").classList.remove("hidden");
  renderTagMgr();
});
$("tm-close").addEventListener("click", () => $("tagmgr").classList.add("hidden"));
$("tagmgr").addEventListener("click", (e) => {
  if (e.target === $("tagmgr")) $("tagmgr").classList.add("hidden");
});

// ---------- Статистика и здоровье библиотеки ----------
function healthBar(list, total) {
  const max = Math.max(1, ...list.map((x) => x.count));
  return list.map((x) =>
    `<div class="h-row"><span class="h-name" title="${esc(x.name)}">${esc(x.name)}</span>` +
    `<span class="h-bar"><i style="width:${(x.count / max) * 100}%"></i></span>` +
    `<span class="h-num">${x.count}</span></div>`
  ).join("") || `<div class="muted small">пусто</div>`;
}

// Переход в галерею по фильтру из карточки здоровья
function healthJump(query, recent = false) {
  $("health").classList.add("hidden");
  state.recent = recent;
  state.collection = null;
  state.similarTo = null;
  // Счётчики здоровья не учитывают SFW/NSFW-фильтр — показываем все рейтинги,
  // иначе «5 без коллекций», а по «показать» пусто (арты оказались NSFW при
  // активном SFW). В режиме тотального скрытия остаёмся на SFW.
  if (!state.hideNsfw) state.nsfw = null;
  renderSimilarChip();
  $("search").value = query;
  renderCollectionsBar();
  doSearch(query);
}

async function openHealth() {
  $("health").classList.remove("hidden");
  $("health-body").innerHTML = "Считаю…";
  let h;
  try { h = await invoke("library_health"); }
  catch (e) { $("health-body").textContent = "Ошибка: " + e; return; }

  const problems = [
    { n: "Без тегов", v: h.untagged, q: "untagged", hint: "Проиндексируй 🧠 или добавь теги вручную" },
    { n: "Без коллекций", v: h.uncollected, q: "uncollected", hint: "🪄 Авто-распределение разложит по коллекциям" },
    { n: "Не проиндексировано ИИ", v: h.unindexed, q: null, hint: "🧠 → Проиндексировать библиотеку" },
    { n: "Без превью", v: h.no_thumb, q: null, hint: "Настройки → Хранение → «Починить битые импорты»" },
    { n: "Битые ссылки (in-place)", v: h.broken_inplace, q: null, hint: "Оригиналы перемещены или удалены с диска" },
  ];

  $("health-body").innerHTML =
    `<div class="h-cards">
      <div class="h-card"><b>${h.files}</b><span>артов</span></div>
      <div class="h-card"><b>${h.collections}</b><span>коллекций</span></div>
      <div class="h-card"><b>${h.groups}</b><span>групп (${h.grouped} артов)</span></div>
      <div class="h-card"><b>${h.tags_total}</b><span>уникальных тегов</span></div>
      <div class="h-card"><b>${h.viewed}</b><span>просмотрено</span></div>
    </div>
    <div class="side-label" style="margin:16px 0 8px;">Требует внимания</div>
    <div id="h-problems">` +
      problems.map((p, i) =>
        `<div class="h-prob ${p.v ? "" : "ok"}">
          <span class="h-prob-n">${p.n}</span>
          <b>${p.v}</b>
          ${p.v && p.q ? `<button class="secondary small-btn" data-hq="${i}">показать</button>` : ""}
          <span class="muted small">${p.v ? esc(p.hint) : "всё хорошо ✓"}</span>
        </div>`).join("") +
    `</div>
    <div class="h-cols">
      <div><div class="side-label" style="margin:16px 0 8px;">Топ тегов</div>${healthBar(h.top_tags)}</div>
      <div><div class="side-label" style="margin:16px 0 8px;">Топ авторов</div>${healthBar(h.top_artists)}</div>
    </div>
    <div class="h-cols">
      <div><div class="side-label" style="margin:16px 0 8px;">Пополнение по месяцам</div>${healthBar(h.by_month)}</div>
      <div><div class="side-label" style="margin:16px 0 8px;">По типам</div>${healthBar(h.by_kind)}</div>
    </div>`;

  $("health-body").querySelectorAll("[data-hq]").forEach((btn) =>
    btn.addEventListener("click", () => healthJump(problems[Number(btn.dataset.hq)].q)));
}

$("btn-health").addEventListener("click", openHealth);
$("health-close").addEventListener("click", () => $("health").classList.add("hidden"));
$("health").addEventListener("click", (e) => {
  if (e.target === $("health")) $("health").classList.add("hidden");
});
$("health-clear-views").addEventListener("click", async () => {
  const yes = await tauriDialog.ask("Очистить историю просмотров?", { title: "FurryManager", kind: "warning" });
  if (!yes) return;
  try {
    await invoke("clear_view_history");
    if (state.recent) { state.recent = false; renderCollectionsBar(); loadMore(true); }
    openHealth();
  } catch (e) { alert(e); }
});

// ---------- Приватность: шторка и ПИН ----------
let uiLocked = false;

function showLock() {
  // ПИН включён — вход по ПИН; иначе просто кнопка «Показать»
  uiLocked = true;
  $("lock").classList.remove("hidden");
  $("lock-err").textContent = "";
  $("lock-recovery").classList.add("hidden");
  const needPin = state.pinEnabled;
  $("lock-pin-row").classList.toggle("hidden", !needPin);
  $("lock-show").classList.toggle("hidden", needPin);
  $("lock-forgot").classList.toggle("hidden", !needPin);
  if (needPin) {
    $("lock-pin").value = "";
    setTimeout(() => $("lock-pin").focus(), 50);
  }
}
function hideLock() {
  uiLocked = false;
  $("lock").classList.add("hidden");
}
async function tryUnlock() {
  const pin = $("lock-pin").value;
  if (!pin) return;
  try {
    const ok = await invoke("pin_verify", { pin });
    if (ok) hideLock();
    else {
      $("lock-err").textContent = "Неверный ПИН";
      $("lock-pin").value = "";
    }
  } catch (e) { $("lock-err").textContent = String(e); }
}
$("lock-enter").addEventListener("click", tryUnlock);
$("lock-pin").addEventListener("keydown", (e) => { if (e.key === "Enter") tryUnlock(); });
$("lock-show").addEventListener("click", hideLock);

// Забыл ПИН: код восстановления или отложенный сброс (24 ч)
$("lock-forgot").addEventListener("click", () => {
  $("lock-recovery").classList.toggle("hidden");
});
$("lock-rec-btn").addEventListener("click", async () => {
  const code = $("lock-rec-code").value.trim();
  if (!code) return;
  try {
    const ok = await invoke("pin_recover", { code });
    if (ok) {
      state.pinEnabled = false;
      hideLock();
      alert("ПИН сброшен по коду восстановления. Задай новый в Настройках → Приватность.");
    } else {
      $("lock-err").textContent = "Код не подошёл";
    }
  } catch (e) { $("lock-err").textContent = String(e); }
});
$("lock-delay-btn").addEventListener("click", async () => {
  try {
    const secs = await invoke("pin_request_reset");
    $("lock-delay-info").textContent =
      `Сброс запрошен: сработает через ${Math.ceil(secs / 3600)} ч, если ПИН не будет введён. Ввод ПИН отменяет сброс.`;
  } catch (e) { $("lock-err").textContent = String(e); }
});

// Скрытие библиотеки: hidden — сворачивание/Win+L; blur — любой расфокус.
// WebView2 при сворачивании не всегда ставит document.hidden, поэтому после
// blur дополнительно спрашиваем у окна isMinimized(); Win+L ловит Rust-детектор.
function maybeLock(fromHidden) {
  if (uiLocked || !state.ready) return;
  const m = state.lockMode;
  if (m === "off") return;
  if (m === "hidden" && !fromHidden) return; // переключение окон не триггерит
  showLock();
}
window.addEventListener("blur", () => {
  if (uiLocked || !state.ready || state.lockMode === "off") return;
  if (state.lockMode === "blur") { showLock(); return; }
  // Режим «при сворачивании»: проверяем чуть позже, свернулись ли реально
  setTimeout(async () => {
    if (uiLocked) return;
    let minimized = false;
    try { minimized = await window.__TAURI__.window.getCurrentWindow().isMinimized(); } catch {}
    if (minimized || document.hidden) showLock();
  }, 250);
});
document.addEventListener("visibilitychange", () => {
  if (document.hidden) maybeLock(true);
});
// Win+L из Rust: экран заблокирован — шторка в обоих активных режимах
listen("screen-locked", () => maybeLock(true));

// Мгновенная блокировка: кнопка 🔒 или Ctrl+L (работает и при выключенной шторке)
function lockNow() {
  if (!uiLocked) showLock();
}
$("btn-lock").addEventListener("click", lockNow);

// ПИН в настройках
function renderPinSettings() {
  $("pin-status").textContent = state.pinEnabled
    ? "ПИН включён — запрашивается при каждом запуске."
    : "ПИН выключен.";
  $("pin-old").classList.toggle("hidden", !state.pinEnabled);
  $("pin-off").classList.toggle("hidden", !state.pinEnabled);
  $("set-lockmode").value = state.lockMode || "off";
}
$("pin-save").addEventListener("click", async () => {
  const cur = $("pin-old").value || null;
  const np = $("pin-new").value.trim();
  if (!np) { alert("Введи новый ПИН (минимум 4 символа)"); return; }
  try {
    const r = await invoke("pin_set", { current: cur, newPin: np });
    state.pinEnabled = !!r.enabled;
    $("pin-old").value = ""; $("pin-new").value = "";
    renderPinSettings();
    if (r.recovery) {
      alert(
        "ПИН сохранён ✓\n\nКод восстановления (показывается ОДИН раз — запиши в надёжное место):\n\n" +
        r.recovery +
        "\n\nЕсли забудешь ПИН, этот код сбросит его с экрана блокировки."
      );
    }
  } catch (e) { alert(e); }
});
$("pin-off").addEventListener("click", async () => {
  const cur = $("pin-old").value || null;
  try {
    const r = await invoke("pin_set", { current: cur, newPin: null });
    state.pinEnabled = !!r.enabled;
    $("pin-old").value = ""; $("pin-new").value = "";
    renderPinSettings();
    alert("ПИН выключен.");
  } catch (e) { alert(e); }
});

// ---------- Вводный гайд ----------
const GUIDE_TOTAL = 4;
let guideStep = 1;
function showGuideStep(n) {
  guideStep = n;
  document.querySelectorAll(".guide-slide").forEach((el) =>
    el.classList.toggle("hidden", el.dataset.g !== String(n)));
  $("guide-next").textContent = n >= GUIDE_TOTAL ? "Готово 🚀" : "Дальше →";
  let dots = "";
  for (let i = 1; i <= GUIDE_TOTAL; i++) dots += `<i class="${i === n ? "on" : ""}"></i>`;
  $("guide-dots").innerHTML = dots;
}
function showGuide() {
  $("guide").classList.remove("hidden");
  showGuideStep(1);
}
function closeGuide() {
  $("guide").classList.add("hidden");
  try { localStorage.setItem("fm_guide_seen", "1"); } catch {}
}
$("guide-next").addEventListener("click", () => {
  if (guideStep >= GUIDE_TOTAL) closeGuide();
  else showGuideStep(guideStep + 1);
});
$("guide-skip").addEventListener("click", closeGuide);
$("guide").addEventListener("click", (e) => { if (e.target === $("guide")) closeGuide(); });

// ---------- Автораспределение по коллекциям ----------
let autocolIds = [];      // арты, для которых строим предложение
let autocolGroups = [];   // предложение от бэкенда
const autocolChecked = new Set(); // индексы включённых групп

async function openAutoCollections(ids) {
  autocolIds = ids;
  $("autocol").classList.remove("hidden");
  await previewAutoCollections();
}

async function previewAutoCollections() {
  $("autocol-status").textContent = "Анализирую теги…";
  $("autocol-list").innerHTML = "";
  autocolChecked.clear();
  let res;
  try {
    res = await invoke("auto_collections_preview", {
      fileIds: autocolIds,
      createNew: $("autocol-new").checked,
    });
  } catch (e) {
    $("autocol-status").textContent = "Ошибка: " + e;
    return;
  }
  autocolGroups = res.groups;
  autocolGroups.forEach((_, i) => autocolChecked.add(i)); // по умолчанию всё включено

  const parts = [];
  if (autocolGroups.length) {
    const total = autocolGroups.reduce((s, g) => s + g.file_ids.length, 0);
    parts.push(`Предложений: ${autocolGroups.length} (артов: ${total})`);
  }
  if (res.unmatched.length) parts.push(`не подошли никуда: ${res.unmatched.length}`);
  if (res.no_tags.length) parts.push(`без тегов: ${res.no_tags.length} (проиндексируй 🧠)`);
  $("autocol-status").textContent = parts.length ? parts.join(" · ") : "Раскладывать нечего 🤷";

  $("autocol-list").innerHTML = autocolGroups.map((g, gi) => {
    const thumbs = g.file_ids.slice(0, 8).map((fid) => {
      const c = state.cards.find((x) => x.id === fid);
      const src = c ? mediaSrc(c.thumb || c.path) : "";
      return src ? `<img loading="lazy" src="${src}" alt="" />` : "";
    }).join("");
    const more = g.file_ids.length > 8 ? `<span class="muted small">+${g.file_ids.length - 8}</span>` : "";
    const badge = g.collection_id === null
      ? `<span class="ac-new">новая</span>` : "";
    return `<div class="ac-group">
      <label class="ac-head">
        <input type="checkbox" data-gi="${gi}" checked />
        <b>${esc(g.name)}</b> ${badge}
        <span class="muted small">← ${g.file_ids.length} арт(ов)</span>
      </label>
      <div class="ac-thumbs">${thumbs}${more}</div>
    </div>`;
  }).join("");
  updateAutocolApply();
}

function updateAutocolApply() {
  const n = [...autocolChecked].reduce((s, i) => s + (autocolGroups[i] ? autocolGroups[i].file_ids.length : 0), 0);
  $("autocol-apply").textContent = `Распределить (${n})`;
  $("autocol-apply").disabled = n === 0;
}

$("autocol-list").addEventListener("change", (e) => {
  const cb = e.target.closest("input[type=checkbox][data-gi]");
  if (!cb) return;
  const gi = Number(cb.dataset.gi);
  if (cb.checked) autocolChecked.add(gi); else autocolChecked.delete(gi);
  updateAutocolApply();
});

$("autocol-apply").addEventListener("click", async () => {
  const chosen = [...autocolChecked].map((i) => autocolGroups[i]).filter(Boolean);
  if (!chosen.length) return;
  try {
    for (const g of chosen) {
      let cid = g.collection_id;
      if (cid === null) {
        cid = await invoke("create_collection", { name: g.name });
      }
      await invoke("collection_add_many", { collectionId: cid, fileIds: g.file_ids });
    }
  } catch (e) { return alert(e); }
  $("autocol").classList.add("hidden");
  refreshCollections();
  reloadKeepScroll();
});

$("autocol-new").addEventListener("change", previewAutoCollections);
$("autocol-close").addEventListener("click", () => $("autocol").classList.add("hidden"));
$("autocol").addEventListener("click", (e) => { if (e.target === $("autocol")) $("autocol").classList.add("hidden"); });
$("sel-auto").addEventListener("click", () => {
  if (state.selected.size) openAutoCollections([...state.selected]);
});

// ---------- Группы: создание и редактор ----------
let geItems = [];   // [{id, thumb, path, kind, ext}]
let geGroupId = null;

// «В группу»: добавить выбранные в существующую группу (панель со списком)
$("sel-grp-add").addEventListener("click", async () => {
  const ids = [...state.selected];
  if (!ids.length) return;
  const groups = await invoke("list_groups").catch(() => []);
  if (!groups.length) {
    showSelPanel(`<div class="menu"><div class="menu-item disabled">Групп пока нет — «🧩 Сгруппировать» создаст первую</div></div>`);
    return;
  }
  showSelPanel(`<div class="menu">` +
    `<div class="menu-title">Добавить выбранные в группу:</div>` +
    groups.map((g) =>
      `<div class="menu-item" data-grp="${g.id}">` +
      (g.cover ? `<img class="mi-thumb" src="${mediaSrc(g.cover)}" alt=""/>` : "") +
      `${g.kind === "comic" ? "📖" : "🧬"} ${esc(g.name || "Без имени")} <span class="cnt">${g.count}</span></div>`
    ).join("") +
    `</div>`);
  $("sel-panel").querySelectorAll(".menu-item[data-grp]").forEach((el) =>
    el.addEventListener("click", async () => {
      hideSelPanel();
      await addToGroup(Number(el.dataset.grp), ids);
      clearSelection();
    }));
});

// «Объединить группы»: выделенные стопки → одна группа (в порядке выделения)
$("sel-merge").addEventListener("click", async () => {
  const gids = [];
  for (const id of state.selected) {
    const c = state.cards.find((x) => x.id === id);
    if (c && c.group_id && !gids.includes(c.group_id)) gids.push(c.group_id);
  }
  if (gids.length < 2) { alert("Выдели минимум две разные группы"); return; }
  const yes = await tauriDialog.ask(
    `Объединить ${gids.length} групп(ы) в одну? Страницы присоединятся в порядке выделения; имя возьмётся от первой (или первой с именем).`,
    { title: "FurryManager", kind: "warning" });
  if (!yes) return;
  try {
    const target = await invoke("groups_merge", { groupIds: gids });
    clearSelection();
    await reloadKeepScroll();
    openGroupEditor(target); // сразу проверить порядок/имя
  } catch (e) { alert(e); }
});

$("sel-group").addEventListener("click", async () => {
  const ids = [...state.selected]; // порядок выделения = стартовый порядок страниц
  if (ids.length < 2) { alert("Выдели минимум два арта (Ctrl+клик)"); return; }
  // Арты, уже состоящие в группах, не смешиваем
  const already = ids.filter((id) => {
    const c = state.cards.find((x) => x.id === id);
    return c && c.group_id;
  });
  if (already.length) {
    alert("Часть выбранных уже в группах — сначала исключи их оттуда (открой группу → Редактировать).");
    return;
  }
  try {
    const gid = await invoke("group_create", { fileIds: ids, kind: "variants", name: null });
    clearSelection();
    openGroupEditor(gid); // сразу настроить: тип, имя, порядок
  } catch (e) { alert(e); }
});

async function openGroupEditor(gid) {
  let g;
  try { g = await invoke("group_get", { id: gid }); }
  catch (e) { return alert(e); }
  geGroupId = gid;
  geItems = g.files.map((f) => ({ id: f.id, thumb: f.thumb, path: f.path, kind: f.kind, ext: f.ext }));
  $("ge-name").value = g.name || "";
  $("ge-kind").value = g.kind || "variants";
  renderGeList();
  $("groupedit").classList.remove("hidden");
}

function renderGeList() {
  const last = geItems.length - 1;
  $("ge-list").innerHTML = geItems.map((f, i) => {
    const src = f.thumb || (f.kind !== "video" ? f.path : "");
    const img = src ? `<img loading="lazy" src="${mediaSrc(src)}" alt=""/>` : `<div style="width:52px;height:52px;display:flex;align-items:center;justify-content:center;">🎬</div>`;
    return `<div class="ge-row" data-i="${i}">
      <span class="ge-pos">${i + 1}</span>${img}
      <span class="ge-meta">#${f.id} · ${esc(f.ext)}</span>
      <button data-act="up" title="Выше" ${i === 0 ? "disabled" : ""}>↑</button>
      <button data-act="down" title="Ниже" ${i === last ? "disabled" : ""}>↓</button>
      <button data-act="rm" title="Исключить из группы (арт останется в библиотеке)">✕</button>
    </div>`;
  }).join("");
}

$("ge-list").addEventListener("click", (e) => {
  const btn = e.target.closest("button[data-act]");
  if (!btn) return;
  const i = Number(btn.closest(".ge-row").dataset.i);
  const act = btn.dataset.act;
  if (act === "up" && i > 0) {
    [geItems[i - 1], geItems[i]] = [geItems[i], geItems[i - 1]];
  } else if (act === "down" && i < geItems.length - 1) {
    [geItems[i + 1], geItems[i]] = [geItems[i], geItems[i + 1]];
  } else if (act === "rm") {
    geItems.splice(i, 1);
  }
  renderGeList();
});

$("ge-save").addEventListener("click", async () => {
  try {
    await invoke("group_update", {
      id: geGroupId,
      name: $("ge-name").value.trim(),
      kind: $("ge-kind").value,
      orderedIds: geItems.map((f) => f.id),
    });
  } catch (e) { return alert(e); }
  $("groupedit").classList.add("hidden");
  if (state.g && state.g.id === geGroupId) closeViewer(); // состав менялся — переоткрыть чище
  reloadKeepScroll();
});

$("ge-dissolve").addEventListener("click", async () => {
  const yes = await tauriDialog.ask(
    "Распустить группу? Арты останутся в библиотеке как одиночные.",
    { title: "FurryManager", kind: "warning" });
  if (!yes) return;
  try { await invoke("group_dissolve", { id: geGroupId }); } catch (e) { return alert(e); }
  $("groupedit").classList.add("hidden");
  if (state.g && state.g.id === geGroupId) closeViewer();
  reloadKeepScroll();
});

// Имя комикса с e621 (по первой странице; явный сетевой запрос)
$("ge-e621").addEventListener("click", async () => {
  if (!geItems.length) return;
  const btn = $("ge-e621");
  btn.disabled = true;
  btn.textContent = "🌐 Ищу…";
  try {
    const name = await invoke("comic_name_from_e621", { fileId: geItems[0].id });
    if (name) {
      $("ge-name").value = name;
      $("ge-kind").value = "comic";
    } else {
      alert("На e621 не нашлось (или пост не в пуле) — введи название вручную.");
    }
  } catch (e) {
    alert(e);
  } finally {
    btn.disabled = false;
    btn.textContent = "🌐 Имя с e621";
  }
});

$("ge-close").addEventListener("click", () => $("groupedit").classList.add("hidden"));
$("groupedit").addEventListener("click", (e) => {
  if (e.target === $("groupedit")) $("groupedit").classList.add("hidden");
});
$("btn-group-edit").addEventListener("click", () => {
  if (state.g) openGroupEditor(state.g.id);
});

// ---------- Дубликаты (near-dupe) ----------
let dupeGroups = [];
const dupeMarked = new Set();

async function openDupes() {
  $("dupes").classList.remove("hidden");
  await scanDupes();
}
async function scanDupes() {
  const thr = Number($("dupe-thr").value) || 8;
  $("dupe-status").textContent = "Сканирую…";
  $("dupes-list").innerHTML = "";
  dupeMarked.clear();
  try {
    dupeGroups = await invoke("find_duplicates", { threshold: thr });
  } catch (e) {
    $("dupe-status").textContent = "Ошибка: " + e;
    return;
  }
  renderDupes();
}
function renderDupes() {
  const totalFiles = dupeGroups.reduce((s, g) => s + g.files.length, 0);
  $("dupe-status").textContent = dupeGroups.length
    ? `Групп: ${dupeGroups.length} · файлов в них: ${totalFiles}`
    : "Почти-копий не найдено 🎉";
  $("dupes-list").innerHTML = dupeGroups.map((g, gi) => {
    const items = g.files.map((f, fi) => {
      const marked = dupeMarked.has(f.id);
      return `<div class="dupe-item ${marked ? "marked" : ""}" data-id="${f.id}">` +
        `<img loading="lazy" src="${mediaSrc(f.thumb || f.path)}" alt="" />` +
        (fi === 0 ? `<div class="dupe-best">★ лучший</div>` : "") +
        `<div class="dupe-meta">${f.width}×${f.height} · ${fmtBytes(f.size)} · ${esc(f.ext)}</div>` +
        `<label class="dupe-check"><input type="checkbox" data-id="${f.id}" ${marked ? "checked" : ""}/> удалить</label>` +
        `</div>`;
    }).join("");
    return `<div class="dupe-group">` +
      `<div class="dupe-group-head"><b>${g.files.length} похожих</b> · оставить лучше самый крупный (★)` +
      `<button class="secondary small-btn" data-act="mark-rest" data-gi="${gi}">Отметить все кроме лучшего</button></div>` +
      `<div class="dupe-items">${items}</div></div>`;
  }).join("");
  updateDupeDelBtn();
}
function updateDupeDelBtn() {
  $("dupe-del").textContent = `🗑 Удалить отмеченные (${dupeMarked.size})`;
  $("dupe-del").disabled = dupeMarked.size === 0;
}
$("dupes-list").addEventListener("change", (e) => {
  const cb = e.target.closest("input[type=checkbox][data-id]");
  if (!cb) return;
  const id = Number(cb.dataset.id);
  if (cb.checked) dupeMarked.add(id); else dupeMarked.delete(id);
  const item = cb.closest(".dupe-item");
  if (item) item.classList.toggle("marked", cb.checked);
  updateDupeDelBtn();
});
$("dupes-list").addEventListener("click", (e) => {
  const btn = e.target.closest("button[data-act='mark-rest']");
  if (!btn) return;
  const g = dupeGroups[Number(btn.dataset.gi)];
  if (!g) return;
  g.files.forEach((f, fi) => { if (fi > 0) dupeMarked.add(f.id); });
  renderDupes();
});
$("dupe-del").addEventListener("click", async () => {
  if (!dupeMarked.size) return;
  const ids = [...dupeMarked];
  const yes = await tauriDialog.ask(
    `Удалить ${ids.length} файл(ов) из библиотеки? Оригиналы на диске (если ещё есть) не трогаются, но копии в библиотеке исчезнут.`,
    { title: "FurryManager", kind: "warning" });
  if (!yes) return;
  try {
    await invoke("delete_files", { fileIds: ids });
  } catch (e) { return alert(e); }
  dupeMarked.clear();
  refreshStats();
  loadMore(true);
  await scanDupes(); // пересканируем — группы обновятся
});
$("dupe-thr").addEventListener("change", scanDupes);
$("dupe-rescan").addEventListener("click", scanDupes);

// ---------- Автосбор групп (вариации по phash, комиксы по названию источника) ----------
let agProposals = [];
const agChecked = new Set();
let agExcluded = new Map(); // gi -> Set(fid): выброшенные из предложения арты

$("dupe-autogroup").addEventListener("click", async () => {
  $("dupes").classList.add("hidden");
  $("autogroups").classList.remove("hidden");
  $("ag-status").textContent = "Ищу вариации и комиксы…";
  $("ag-list").innerHTML = "";
  agChecked.clear();
  agExcluded = new Map();
  try {
    agProposals = await invoke("auto_groups_preview");
  } catch (e) {
    $("ag-status").textContent = "Ошибка: " + e;
    return;
  }
  agProposals.forEach((_, i) => agChecked.add(i));
  renderAgList();
});

function renderAgList() {
  if (!agProposals.length) {
    $("ag-status").textContent = "Кандидатов не нашлось — всё уже собрано 🎉";
    $("ag-list").innerHTML = "";
    updateAgApply();
    return;
  }
  const comics = agProposals.filter((p) => p.kind === "comic").length;
  $("ag-status").textContent =
    `Предложений: ${agProposals.length} (комиксов: ${comics}, вариаций: ${agProposals.length - comics})`;
  $("ag-all").checked = agChecked.size === agProposals.length;
  $("ag-list").innerHTML = agProposals.map((p, i) => {
    const ex = agExcluded.get(i) || new Set();
    // Превью присылает бэкенд; клик по миниатюре исключает арт из группы
    const thumbs = (p.thumbs || []).map((t, k) => {
      const fid = p.file_ids[k];
      return `<img loading="lazy" class="ag-th ${ex.has(fid) ? "off" : ""}" data-gi="${i}" data-fid="${fid}" src="${mediaSrc(t)}" alt="" title="Клик — исключить/вернуть"/>`;
    }).join("");
    const kept = p.file_ids.filter((fid) => !ex.has(fid)).length;
    const label = p.kind === "comic"
      ? `📖 Комикс${p.name ? `: «${esc(p.name)}»` : ""}`
      : `🧬 Вариации`;
    const cnt = ex.size ? `${kept} из ${p.file_ids.length}` : `${p.file_ids.length} арт(ов)`;
    return `<div class="ac-group">
      <label class="ac-head">
        <input type="checkbox" data-gi="${i}" ${agChecked.has(i) ? "checked" : ""} />
        <b>${label}</b>
        <span class="muted small" data-cnt="${i}">← ${cnt}</span>
      </label>
      <div class="ac-thumbs">${thumbs}</div>
    </div>`;
  }).join("");
  updateAgApply();
}

// Клик по миниатюре — исключить арт из предложения (или вернуть)
$("ag-list").addEventListener("click", (e) => {
  const img = e.target.closest("img.ag-th");
  if (!img) return;
  const gi = Number(img.dataset.gi);
  const fid = Number(img.dataset.fid);
  if (!agExcluded.has(gi)) agExcluded.set(gi, new Set());
  const ex = agExcluded.get(gi);
  if (ex.has(fid)) { ex.delete(fid); img.classList.remove("off"); }
  else { ex.add(fid); img.classList.add("off"); }
  const p = agProposals[gi];
  const kept = p.file_ids.filter((f) => !ex.has(f)).length;
  const cntEl = $("ag-list").querySelector(`[data-cnt="${gi}"]`);
  if (cntEl) cntEl.textContent = `← ${ex.size ? `${kept} из ${p.file_ids.length}` : `${p.file_ids.length} арт(ов)`}`;
});

// Мастер-галочка: включить/выключить все предложения разом
$("ag-all").addEventListener("change", () => {
  agChecked.clear();
  if ($("ag-all").checked) agProposals.forEach((_, i) => agChecked.add(i));
  renderAgList();
});

function updateAgApply() {
  const n = agChecked.size;
  $("ag-apply").textContent = `Сгруппировать (${n})`;
  $("ag-apply").disabled = n === 0;
  $("ag-all").checked = agProposals.length > 0 && n === agProposals.length;
}

$("ag-list").addEventListener("change", (e) => {
  const cb = e.target.closest("input[type=checkbox][data-gi]");
  if (!cb) return;
  const i = Number(cb.dataset.gi);
  if (cb.checked) agChecked.add(i); else agChecked.delete(i);
  updateAgApply();
});

$("ag-apply").addEventListener("click", async () => {
  const chosen = [...agChecked];
  if (!chosen.length) return;
  let skipped = 0;
  try {
    for (const gi of chosen) {
      const p = agProposals[gi];
      if (!p) continue;
      const ex = agExcluded.get(gi) || new Set();
      const ids = p.file_ids.filter((fid) => !ex.has(fid));
      if (ids.length < 2) { skipped++; continue; } // после выбраковки группы не вышло
      await invoke("group_create", {
        fileIds: ids,
        kind: p.kind,
        name: p.name || null,
      });
    }
  } catch (e) { return alert(e); }
  if (skipped) alert(`Пропущено предложений: ${skipped} (после исключений осталось меньше двух артов).`);
  $("autogroups").classList.add("hidden");
  refreshStats();
  reloadKeepScroll();
});

$("ag-close").addEventListener("click", () => $("autogroups").classList.add("hidden"));
$("autogroups").addEventListener("click", (e) => {
  if (e.target === $("autogroups")) $("autogroups").classList.add("hidden");
});
$("btn-dupes").addEventListener("click", openDupes);
$("dupes-close").addEventListener("click", () => $("dupes").classList.add("hidden"));
$("dupes").addEventListener("click", (e) => { if (e.target === $("dupes")) $("dupes").classList.add("hidden"); });

// ---------- Онбординг ----------
let obFolder = null;
function obShow(step) {
  document.querySelectorAll(".ob-step").forEach((el) =>
    el.classList.toggle("hidden", el.dataset.step !== String(step)));
}
document.querySelectorAll(".ob-next").forEach((btn) =>
  btn.addEventListener("click", (e) => {
    const cur = Number(e.target.closest(".ob-step").dataset.step);
    obShow(cur + 1);
  }));
$("ob-choose").addEventListener("click", async () => {
  const folder = await tauriDialog.open({
    directory: true, title: "Папка для библиотеки FurryManager",
  });
  if (folder) {
    obFolder = folder;
    $("ob-chosen").textContent = "📁 " + folder;
    $("ob-next-2").disabled = false;
  }
});
$("ob-finish").addEventListener("click", async () => {
  const mode = document.querySelector('input[name="ob-mode"]:checked').value;
  try {
    await invoke("setup_library", { path: obFolder, mode });
    applyMode(mode);
    $("onboarding").classList.add("hidden");
    refreshStats();
    refreshCollections();
    loadMore(true);
    showGuide(); // сразу краткий вводный тур
  } catch (e) { alert(e); }
});

// ---------- Старт ----------
// Аварийный экран старта вместо «белизны».
function showBootError(html) {
  $("boot-error-msg").innerHTML = html;
  $("boot-error").classList.remove("hidden");
}
$("boot-retry").addEventListener("click", () => location.reload());
$("boot-pick").addEventListener("click", async () => {
  try {
    if (await chooseLibraryFolder("Укажи папку библиотеки")) location.reload();
  } catch (e) { alert(e); }
});
// Ограничитель ожидания: недоступная библиотека (сетевой/внешний диск) может
// отвечать десятки секунд — не ждём молча с белым экраном.
function withTimeout(promise, ms, label) {
  return Promise.race([
    promise,
    new Promise((_, rej) => setTimeout(() => rej(new Error(label || "timeout")), ms)),
  ]);
}

async function boot() {
  // Визуальные настройки — до запроса к бэкенду, чтобы не мигало темой
  applyDisplay(loadDisplay());
  try { state.sort = localStorage.getItem("fm_sort") || null; } catch {}
  if (state.sort) $("sort-select").value = state.sort;
  if (state.sort === "random") newRandSeed();
  updateSortUi();
  const setup = await withTimeout(
    invoke("get_setup_state"), 8000, "Библиотека долго не отвечает"
  );
  applyMode(setup.mode || "simple");
  state.nsfwSplit = !!setup.nsfw_split;
  state.lossless = !!setup.lossless;
  state.lan = !!setup.lan;
  state.pinEnabled = !!setup.pin_enabled;
  state.lockMode = setup.lock_mode || "off";
  state.browserCookies = setup.browser_cookies || "";
  state.appVersion = setup.app_version || "";
  state.libraryRoot = setup.library_root || "";
  if (state.pinEnabled) {
    showLock(); // шторка сразу, контент грузится под ней
    if (setup.pin_reset_remaining) {
      $("lock-recovery").classList.remove("hidden");
      $("lock-delay-info").textContent =
        `Запрошен сброс ПИН: сработает через ~${Math.ceil(setup.pin_reset_remaining / 3600)} ч. Ввод ПИН отменит его.`;
    }
  }
  state.defaultNsfw = setup.default_nsfw ?? null;
  if (state.nsfwSplit) state.nsfw = state.defaultNsfw; // фильтр при запуске
  state.hideNsfw = !!setup.hide_nsfw;
  if (state.hideNsfw) state.nsfw = 0; // режим скрытия — залочено на SFW
  state.gifAuto = !!setup.video_gif;
  state.gifSecs = setup.video_gif_secs || 5;
  state.gifReplace = !!setup.video_gif_replace;
  state.gifFps = setup.video_gif_fps || 15;
  state.gifWidth = setup.video_gif_width || 480;
  state.useTrash = setup.use_trash !== false;
  state.trashDays = setup.trash_days ?? 30;
  state.httpToken = setup.http_token;
  state.httpPort = setup.http_port;
  state.ready = true; // фильтры установлены — можно грузить галерею
  if (setup.library_root && !setup.library_ok) {
    // Папка была, но исчезла (переименовали, вынули диск) — не молчим
    state.ready = true;
    alert(
      "Папка библиотеки недоступна:\n" + setup.library_root +
      "\n\nВозможно, её переместили или диск не подключён. Укажи папку заново — " +
      "все теги и коллекции лежат внутри неё и подхватятся."
    );
    if (!(await chooseLibraryFolder("Укажи папку библиотеки заново"))) {
      $("onboarding").classList.remove("hidden");
      obShow(2);
    }
    return;
  }
  if (!setup.library_root) {
    $("onboarding").classList.remove("hidden");
    obShow(1);
  } else {
    refreshStats();
    refreshCollections();
    loadMore(true);
    // Существующим пользователям показываем гайд один раз.
    let seen = false;
    try { seen = localStorage.getItem("fm_guide_seen") === "1"; } catch {}
    if (!seen) showGuide();
  }
}

(async function init() {
  try {
    await boot();
  } catch (e) {
    console.error("boot failed:", e);
    showBootError(
      "Не удалось загрузить библиотеку.<br><br>" +
      "Скорее всего, папка с библиотекой сейчас недоступна — внешний или сетевой " +
      "диск не подключён, либо компьютер с библиотекой выключен. Все арты и теги " +
      "лежат в этой папке; как только она снова доступна, всё вернётся на место.<br><br>" +
      "<span class=\"small\">" + esc(String((e && e.message) || e)) + "</span>"
    );
  }
})();
