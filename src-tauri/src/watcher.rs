//! Слежение за папками: новые/изменённые файлы автоматически импортируются.
//! Один notify-watcher на всё приложение; события дебаунсятся и обрабатываются
//! в отдельном потоке с собственным соединением с БД (WAL допускает параллель).

use crate::commands::load_config;
use crate::{db, import};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Живой watcher хранится здесь (если уронить — слежение прекращается).
/// Arc — чтобы подписку на папки можно было унести в фоновый поток.
pub struct WatchState {
    watcher: Arc<Mutex<Option<RecommendedWatcher>>>,
}

impl WatchState {
    pub fn new() -> Self {
        WatchState {
            watcher: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for WatchState {
    fn default() -> Self {
        Self::new()
    }
}

/// Создаёт watcher, запускает обработчик и подписывается на папки из конфига,
/// плюс первичный скан (подхватить файлы, добавленные при выключенном приложении).
pub fn init(app: &AppHandle, state: &WatchState) {
    let watcher = match build_watcher(app.clone()) {
        Some(w) => w,
        None => {
            eprintln!("[watcher] не удалось создать наблюдатель");
            return;
        }
    };
    *state.watcher.lock().unwrap() = Some(watcher);

    // Подписка на папки и первичный скан — в фоне. w.watch() на недоступной
    // сетевой/облачной папке (например, iCloud без интернета) может подвиснуть,
    // а init() вызывается из setup-хука: блокировка здесь = белое окно на старте.
    let arc = state.watcher.clone();
    let app2 = app.clone();
    std::thread::spawn(move || {
        for wf in load_config().watched {
            if let Some(w) = arc.lock().unwrap().as_mut() {
                let _ = w.watch(Path::new(&wf.path), RecursiveMode::Recursive);
            }
            initial_scan(app2.clone(), wf.path.clone(), wf.in_place);
        }
    });
}

fn build_watcher(app: AppHandle) -> Option<RecommendedWatcher> {
    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
    let watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .ok()?;
    std::thread::spawn(move || process_loop(app, rx));
    Some(watcher)
}

/// Подписаться на папку (рекурсивно).
pub fn watch_path(state: &WatchState, path: &str) {
    if let Some(w) = state.watcher.lock().unwrap().as_mut() {
        let _ = w.watch(Path::new(path), RecursiveMode::Recursive);
    }
}

/// Отписаться от папки.
pub fn unwatch_path(state: &WatchState, path: &str) {
    if let Some(w) = state.watcher.lock().unwrap().as_mut() {
        let _ = w.unwatch(Path::new(path));
    }
}

/// Первичный импорт содержимого папки в фоне.
pub fn initial_scan(app: AppHandle, path: String, in_place: bool) {
    std::thread::spawn(move || {
        let cfg = load_config();
        let lossless = cfg.lossless.unwrap_or(false);
        let root = match cfg.library_root {
            Some(r) => PathBuf::from(r),
            None => return,
        };
        let inputs = [PathBuf::from(&path)];
        // honor_deleted: не возвращать арты, которые пользователь удалил из библиотеки
        if let Ok(sum) = import::run(&root, &inputs, in_place, lossless, true, |_| {}) {
            if sum.imported > 0 {
                let _ = app.emit(
                    "watch-import",
                    serde_json::json!({ "added": sum.imported, "folder": path }),
                );
            }
        }
    });
}

fn process_loop(app: AppHandle, rx: Receiver<notify::Result<Event>>) {
    loop {
        // Ждём первое событие (блокирующе). Ошибка канала = watcher уронили.
        let first = match rx.recv() {
            Ok(r) => r,
            Err(_) => return,
        };
        let mut paths: HashSet<PathBuf> = HashSet::new();
        collect(first, &mut paths);

        // Дебаунс: копим события ещё немного (файлы часто пишутся порциями).
        let deadline = Instant::now() + Duration::from_millis(1500);
        while let Ok(r) = rx.recv_timeout(Duration::from_millis(400)) {
            collect(r, &mut paths);
            if Instant::now() >= deadline {
                break;
            }
        }
        if !paths.is_empty() {
            import_batch(&app, paths);
        }
    }
}

/// Ждём стабильности файла: два одинаковых ненулевых замера размера подряд.
/// false — файл исчез (перемещён/удалён до импорта).
fn wait_stable(p: &Path) -> bool {
    let mut last: Option<u64> = None;
    for _ in 0..20 {
        // до ~6 секунд на файл
        let Ok(md) = std::fs::metadata(p) else { return false };
        let sz = md.len();
        if sz > 0 && last == Some(sz) {
            return true;
        }
        last = Some(sz);
        std::thread::sleep(Duration::from_millis(300));
    }
    true // так и растёт — пробуем как есть, починка подстрахует
}

fn collect(res: notify::Result<Event>, out: &mut HashSet<PathBuf>) {
    if let Ok(ev) = res {
        if matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_)) {
            for p in ev.paths {
                out.insert(p);
            }
        }
    }
}

fn import_batch(app: &AppHandle, paths: HashSet<PathBuf>) {
    let cfg = load_config();
    let lossless = cfg.lossless.unwrap_or(false);
    let root = match cfg.library_root {
        Some(r) => PathBuf::from(r),
        None => return,
    };
    let conn = match db::open(&root.join("library.db")) {
        Ok(c) => c,
        Err(_) => return,
    };
    let watched = cfg.watched;
    let mut added = 0i64;
    for p in paths {
        if !p.is_file() {
            continue;
        }
        // К какой отслеживаемой папке относится файл (и её режим хранения).
        let wf = match watched.iter().find(|w| p.starts_with(&w.path)) {
            Some(w) => w,
            None => continue,
        };
        // Файл может ещё копироваться (USB/сеть) — ждём, пока размер устаканится,
        // иначе импортируем обрезок без превью.
        if !wait_stable(&p) {
            continue;
        }
        if let Ok((_, true)) = import::import_one(&conn, &root, &p, wf.in_place, lossless, true) {
            added += 1;
        }
    }
    if added > 0 {
        let _ = app.emit("watch-import", serde_json::json!({ "added": added }));
    }
}
