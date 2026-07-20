const $ = (id) => document.getElementById(id);

chrome.storage.sync.get({ token: "" }).then((s) => {
  if (!s.token) {
    $("status").innerHTML = '<span class="warn">⚠ Токен не задан — открой Настройки и вставь токен из FurryManager (⚙).</span>';
  }
});

$("save").addEventListener("click", async () => {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab || !tab.url) return;
  $("status").textContent = "Отправляю…";
  const resp = await chrome.runtime.sendMessage({ type: "save", url: tab.url });
  $("status").textContent = resp && resp.ok
    ? "✓ Отправлено! Прогресс — в приложении."
    : "✗ " + ((resp && resp.error) || "не удалось");
});

$("options").addEventListener("click", () => chrome.runtime.openOptionsPage());
