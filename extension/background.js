// FurryManager Saver — фоновый воркер.
// Шлёт URL в локальное приложение: POST http://127.0.0.1:<port>/ingest

async function getSettings() {
  return chrome.storage.sync.get({ token: "", port: 7877 });
}

async function sendToManager(url, tabId) {
  const { token, port } = await getSettings();
  if (!token) {
    notify("Сначала вставь токен: клик по иконке расширения → Настройки. Токен — в FurryManager: ⚙ Настройки.");
    return { ok: false, error: "нет токена" };
  }
  try {
    const resp = await fetch(`http://127.0.0.1:${port}/ingest`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ token, url }),
    });
    if (resp.ok) {
      if (tabId) badge(tabId, "✓", "#2e9e5b");
      notify("⏳ Отправлено! Скачивание идёт — прогресс в приложении FurryManager.");
      return { ok: true };
    }
    const data = await resp.json().catch(() => ({}));
    if (tabId) badge(tabId, "✗", "#c33");
    notify("FurryManager отказал: " + (data.error || resp.status));
    return { ok: false, error: data.error || String(resp.status) };
  } catch (e) {
    if (tabId) badge(tabId, "✗", "#c33");
    notify("Не удалось подключиться к FurryManager (127.0.0.1:" + port + "). Приложение запущено?");
    return { ok: false, error: "приложение не запущено?" };
  }
}

function badge(tabId, text, color) {
  chrome.action.setBadgeText({ text, tabId });
  chrome.action.setBadgeBackgroundColor({ color, tabId });
  setTimeout(() => chrome.action.setBadgeText({ text: "", tabId }), 4000);
}

function notify(message) {
  chrome.notifications.create({
    type: "basic",
    iconUrl: "icons/128.png",
    title: "FurryManager",
    message,
  });
}

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({
    id: "fm-save",
    title: "🦊 Сохранить в FurryManager",
    contexts: ["page", "image", "video", "link"],
  });
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  // Для страницы поста шлём URL страницы (gallery-dl вытащит оригинал и метаданные),
  // для прямой ссылки/картинки — её адрес.
  const url = info.linkUrl || info.pageUrl || info.srcUrl;
  if (url) sendToManager(url, tab && tab.id);
});

// Запросы из попапа
chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg && msg.type === "save" && msg.url) {
    sendToManager(msg.url).then(sendResponse);
    return true; // асинхронный ответ
  }
});
