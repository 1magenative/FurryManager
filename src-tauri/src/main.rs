// Не показывать консольное окно в release-сборке на Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod db;
mod heic;
mod import;
mod server;
mod sidecar;
mod similar;
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

fn main() {
    // Если библиотека уже настроена — открываем базу сразу.
    let cfg = commands::load_config();
    let root: Option<PathBuf> = cfg.library_root.as_ref().map(PathBuf::from);
    let conn = root
        .as_ref()
        .and_then(|r| db::open(&r.join("library.db")).ok());

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
        .manage(watcher::WatchState::new())
        .setup(move |app| {
            let handle = app.handle().clone();
            server::start(handle.clone(), http_port, http_token.clone(), lan);
            // Наблюдение за папками из конфига + первичный скан.
            let ws = app.state::<watcher::WatchState>();
            watcher::init(&handle, ws.inner());
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
        ])
        .run(tauri::generate_context!())
        .expect("Не удалось запустить приложение");
}
