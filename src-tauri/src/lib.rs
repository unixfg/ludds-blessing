mod commands;
mod core_service;
pub mod error;
pub mod game_settings;
pub mod models;
mod service;

use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            app.manage(service::AppState::new(app_data_dir)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::discover_installations,
            commands::register_root,
            commands::forget_root,
            commands::list_game_settings_profiles,
            commands::load_game_settings,
            commands::save_game_settings_profile,
            commands::delete_game_settings_profile,
            commands::apply_game_settings,
            commands::scan_saves,
            commands::open_save,
            commands::close_session,
            commands::load_portrait,
            commands::unlock_protected_save,
            commands::prepare_review,
            commands::discard_review,
            commands::apply_review,
            commands::list_backups,
            commands::prepare_restore,
            commands::apply_restore,
            commands::startup_recovery_state,
            commands::export_diagnostics,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Ludd's Blessing");
}
