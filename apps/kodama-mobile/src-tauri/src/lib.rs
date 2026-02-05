//! Kodama Mobile - Camera viewer and server for mobile platforms

mod commands;
mod state;

use state::AppState;
use std::sync::Arc;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_state = Arc::new(AppState::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::start_server,
            commands::stop_server,
            commands::get_server_status,
            commands::connect_to_server,
            commands::disconnect,
            commands::get_connection_status,
            commands::get_cameras,
            commands::get_settings,
            commands::update_settings,
            commands::get_app_mode,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
