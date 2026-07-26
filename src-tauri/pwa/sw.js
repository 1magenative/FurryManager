// FurryManager PWA — Service Worker.
// Задача одна: чтобы приложение ОТКРЫВАЛОСЬ офлайн. Сами арты офлайн уже
// показывает страница из IndexedDB (кнопка ⤓). Без SW оболочку (HTML) взять
// офлайн неоткуда — Safari просто не может загрузить страницу без ПК.
//
// Работает только в защищённом контексте (https), который нам даёт Tailscale.
// Меняешь оболочку — подними версию кэша, старый удалится на activate.

const SHELL = 'fm-shell-v2';

self.addEventListener('install', () => self.skipWaiting());

self.addEventListener('activate', (e) => {
  e.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.map((k) => (k === SHELL ? null : caches.delete(k)))))
      .then(() => self.clients.claim())
  );
});

self.addEventListener('fetch', (e) => {
  // ВАЖНО: трогаем ТОЛЬКО навигацию (открытие приложения). Картинки, видео и
  // /api отдаём браузеру НАПРЯМУЮ, не вмешиваясь. Причина: iOS Safari не
  // поддерживает частичные ответы (206 / Range) через Service Worker, и если
  // видео идёт через SW — оно грузится целиком до старта (минуты вместо секунд).
  if (e.request.mode !== 'navigate') return;
  e.respondWith(
    fetch(e.request)
      .then((r) => {
        const copy = r.clone();
        caches.open(SHELL).then((c) => c.put('/', copy)); // свежая оболочка в кэш
        return r;
      })
      .catch(() => caches.match('/', { ignoreSearch: true })) // офлайн — из кэша
  );
});
