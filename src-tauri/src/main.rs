// Не показывать консольное окно в release-сборке на Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod db;
mod heic;
mod import;
mod server;
mod sidecar;
mod similar;
mod tg;
mod watcher;

use commands::{AiState, AppState};
use std::path::PathBuf;
use tauri::Manager; // для app.state()

/// Детектор блокировки экрана (Win+L): у заблокированного рабочего стола
/// GetForegroundWindow возвращает NULL для процессов обычного десктопа.
#[cfg(windows)]
fn start_lock_watcher(app: tauri::AppHandle) {
    use tauri::Emitter;
    std::thread::spawn(move || {
        use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        let mut was_locked = false;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let locked = unsafe { GetForegroundWindow() }.is_null();
            if locked && !was_locked {
                let _ = app.emit("screen-locked", ());
            }
            was_locked = locked;
        }
    });
}
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Режим тихой синхронизации (`--sync`): подхватить новое из отслеживаемых
/// папок и Telegram и закрыться. Интерфейс не поднимаем вообще.
/// Если приложение уже запущено — просто выходим: оно и так всё это делает.
fn run_sync_mode() -> ! {
    let cfg = commands::load_config();
    let port = cfg.http_port.unwrap_or(7877);
    let already = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|c| c.get(format!("http://127.0.0.1:{port}/ping")).send().ok())
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    if already {
        std::process::exit(0);
    }

    let mut added = 0i64;
    if let Some(root) = cfg.library_root.as_ref().map(PathBuf::from) {
        let lossless = cfg.lossless.unwrap_or(false);
        for wf in &cfg.watched {
            let inputs = [PathBuf::from(&wf.path)];
            // honor_deleted: удалённое обратно не тянем
            if let Ok(sum) = import::run(&root, &inputs, wf.in_place, lossless, true, |_| {}) {
                added += sum.imported;
            }
        }
    }
    let tg_added = tg::sync_once();
    eprintln!("[sync] добавлено из папок: {added}, из Telegram: {tg_added}");
    std::process::exit(0);
}

/// Показать и сфокусировать главное окно (из трея или второго запуска).
fn show_main_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn main() {
    // Режимы запуска: --sync (тихо синхронизировать и выйти),
    // --background (свернуть в трей, окно не показывать).
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--sync") {
        run_sync_mode();
    }
    let start_hidden = args.iter().any(|a| a == "--background");

    // Если библиотека уже настроена — открываем базу сразу, но с таймаутом:
    // на отключённом сетевом/внешнем диске db::open висит на таймауте ОС и
    // подвесил бы весь запуск (окно так и не появится — «белый экран»).
    // Не открылось за 3с — стартуем без соединения (команды вернут понятную
    // ошибку, а окно загрузится и покажет экран «папка недоступна»).
    let cfg = commands::load_config();
    let root: Option<PathBuf> = cfg.library_root.as_ref().map(PathBuf::from);
    // Приватность: прячем внутренние папки от Проводника/поиска Windows.
    // Плюс чистим корзину от старья (в фоне — трогает диск).
    if let Some(r) = &root {
        commands::harden_library(r);
        let r2 = r.clone();
        let days = cfg.trash_days.unwrap_or(30);
        std::thread::spawn(move || commands::trash_purge_old(&r2, days));
    }
    let conn = root.as_ref().and_then(|r| {
        let path = r.join("library.db");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(db::open(&path).ok());
        });
        rx.recv_timeout(std::time::Duration::from_secs(3))
            .ok()
            .flatten()
    });

    let (http_port, http_token) = commands::ensure_http_credentials();
    let lan = cfg.lan.unwrap_or(false);

    tauri::Builder::default()
        // Одно окно на приложение: повторный запуск разворачивает уже открытое.
        // Плагин должен идти ПЕРВЫМ.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        // В фоновом режиме крестик прячет окно в трей, а не закрывает приложение
        // (иначе автозапуск «в фоне» терял бы смысл после первого закрытия).
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let bg = commands::load_config().autostart.as_deref() == Some("background");
                if bg {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .manage(watcher::WatchState::new())
        .setup(move |app| {
            let handle = app.handle().clone();

            // Иконка в трее: из фонового режима приложение оттуда и открывается
            {
                use tauri::menu::{Menu, MenuItem};
                use tauri::tray::TrayIconBuilder;
                let show = MenuItem::with_id(app, "tray-show", "Открыть FurryManager", true, None::<&str>)?;
                let quit = MenuItem::with_id(app, "tray-quit", "Выход", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show, &quit])?;
                let mut b = TrayIconBuilder::with_id("main-tray")
                    .menu(&menu)
                    .tooltip("FurryManager")
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "tray-show" => show_main_window(app),
                        "tray-quit" => app.exit(0),
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        // Двойной клик по иконке — открыть окно
                        if matches!(event, tauri::tray::TrayIconEvent::DoubleClick { .. }) {
                            show_main_window(tray.app_handle());
                        }
                    });
                if let Some(icon) = app.default_window_icon() {
                    b = b.icon(icon.clone());
                }
                let _ = b.build(app);
            }

            // Фоновый запуск: окно не показываем, работаем из трея
            if start_hidden {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }

            server::start(handle.clone(), http_port, http_token.clone(), lan);
            // Наблюдение за папками из конфига + первичный скан.
            let ws = app.state::<watcher::WatchState>();
            watcher::init(&handle, ws.inner());
            tg::start_if_enabled(handle.clone()); // Telegram-бот-загрузчик, если включён
            // Удержание системы от сна — если включено в настройках
            commands::apply_keep_awake(commands::load_config().keep_awake.unwrap_or(false));
            #[cfg(windows)]
            start_lock_watcher(handle.clone());
            Ok(())
        })
        .manage(AppState {
            conn: Mutex::new(conn),
            root: Mutex::new(root),
        })
        .manage(AiState {
            sidecar: Arc::new(sidecar::Sidecar::new()),
            indexing: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_setup_state,
            commands::setup_library,
            commands::set_mode,
            commands::import_paths,
            commands::query_files,
            commands::delete_file,
            commands::list_collections,
            commands::create_collection,
            commands::delete_collection,
            commands::rename_collection,
            commands::toggle_collection,
            commands::reorder_collections,
            commands::file_details,
            commands::add_tag,
            commands::add_tag_many,
            commands::suggest_tags,
            commands::remove_tag,
            commands::list_all_tags,
            commands::tag_rename,
            commands::tags_merge,
            commands::tags_delete,
            commands::add_tag_relation,
            commands::list_tag_relations,
            commands::delete_tag_relation,
            commands::delete_files,
            commands::collection_add_many,
            commands::collection_remove_many,
            commands::similar_local_cmd,
            commands::similar_web,
            commands::library_stats,
            commands::ai_pending,
            commands::ai_index,
            commands::ai_cancel,
            commands::ai_setup,
            commands::set_settings,
            commands::export_library,
            commands::import_library,
            commands::pin_set,
            commands::pin_verify,
            commands::pin_recover,
            commands::pin_request_reset,
            commands::groups_merge,
            commands::set_nsfw,
            commands::import_clipboard,
            commands::find_duplicates,
            commands::auto_collections_preview,
            commands::lan_qr,
            commands::repair_heic,
            commands::count_files,
            commands::mark_viewed,
            commands::clear_view_history,
            commands::library_health,
            commands::sidecar_status,
            commands::group_create,
            commands::group_get,
            commands::group_update,
            commands::group_dissolve,
            commands::list_groups,
            commands::group_add_files,
            commands::auto_groups_preview,
            commands::comic_name_from_e621,
            commands::ai_reindex_reset,
            commands::list_videos_for_frames,
            commands::ai_index_video,
            commands::nsfw_reset_all,
            commands::list_watched,
            commands::add_watched,
            commands::remove_watched,
            commands::save_video_thumb,
            commands::semantic_search,
            commands::files_brief,
            commands::ts_status,
            commands::ts_serve_enable,
            commands::ts_serve_disable,
            commands::set_collection_nsfw,
            commands::set_hide_nsfw,
            tg::tg_enable,
            tg::tg_disable,
            tg::tg_status,
            commands::set_favorite,
            commands::trash_list,
            commands::trash_count,
            commands::trash_restore,
            commands::trash_purge,
            commands::set_trash_settings,
            commands::set_keep_awake,
            commands::group_export,
            commands::group_import,
            commands::query_file_ids,
            commands::set_autostart,
            commands::set_gif_settings,
            commands::list_videos_for_gif,
            commands::mark_gif_done,
            commands::reset_gif_done,
            commands::video_to_gif,
        ])
        .run(tauri::generate_context!())
        .expect("Не удалось запустить приложение");
}
