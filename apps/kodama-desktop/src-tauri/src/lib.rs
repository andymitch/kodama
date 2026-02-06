//! Kodama Desktop - Tauri backend library
//!
//! This module provides the Tauri commands and state management for the desktop app.
//! The app supports two modes:
//! - Server mode: Acts as a full server with optional storage
//! - Client mode: Connects to an external server as a viewer

mod commands;
mod state;

use tauri::Manager;
use tracing_subscriber::EnvFilter;

pub use commands::*;
pub use state::*;

/// Initialize the Tauri app
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("kodama=info".parse().unwrap())
                .add_directive("kodama_desktop=debug".parse().unwrap()),
        )
        .init();

    tracing::info!("Kodama Desktop starting");

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Initialize app state
            let state = AppState::new();
            app.manage(state);

            tracing::info!("App state initialized");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Server mode commands
            commands::start_server,
            commands::stop_server,
            commands::get_server_status,
            // Client mode commands
            commands::connect_to_server,
            commands::disconnect,
            commands::get_connection_status,
            // Camera management
            commands::list_cameras,
            commands::get_camera_info,
            // Storage commands
            commands::get_storage_stats,
            commands::configure_storage,
            // Settings
            commands::get_settings,
            commands::save_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
