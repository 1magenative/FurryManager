const $ = (id) => document.getElementById(id);

chrome.storage.sync.get({ token: "", port: 7877 }).then((s) => {
  $("token").value = s.token;
  $("port").value = s.port;
});

$("save").addEventListener("click", async () => {
  await chrome.storage.sync.set({
    token: $("token").value.trim(),
    port: Number($("port").value) || 7877,
  });
  $("status").textContent = "Сохранено ✓";
  setTimeout(() => ($("status").textContent = ""), 2000);
});
