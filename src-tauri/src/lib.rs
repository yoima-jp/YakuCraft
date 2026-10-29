pub mod commands;
pub mod error;
pub mod logging;
pub mod paths;
pub mod state;

use tauri::Manager;

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let app_local_data_dir = app.path().app_local_data_dir()?;
            let paths = paths::AppPaths::create(app_local_data_dir)?;
            let settings = match yc_project::load_settings(&paths.settings_file) {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!("設定を読み込めないため、Debug logsを無効にします: {error}");
                    yc_project::AppSettings::default()
                }
            };
            let debug_logs = settings
                .values
                .get("advanced.debugLogs")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let log_guard = logging::init(&paths.logs_dir, debug_logs)?;
            if let Err(error) = yc_project::cleanup_trash_directories(&paths.projects_dir) {
                tracing::warn!(%error, "deferred Project deletion sweep could not inspect projects directory");
            }
            app.manage(state::AppState::new(paths, log_guard));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app::get_app_info,
            commands::app::exit_app,
            commands::project::create_project_from_jars,
            commands::project::list_mod_units,
            commands::project::refresh_modpack_unit,
            commands::project::cancel_analysis,
            commands::project::is_shaderpack_folder,
            commands::project::scan_mods_folder,
            commands::project::scan_project_instance_sources,
            commands::project::add_mod_units,
            commands::project::get_project_summary,
            commands::project::get_review_asset_summary,
            commands::project::save_review_settings,
            commands::project::open_project,
            commands::project::list_recent_projects,
            commands::project::rename_project,
            commands::project::get_mod_unit_removal_info,
            commands::project::remove_mod_unit,
            commands::project::delete_project,
            commands::editor::list_entries,
            commands::editor::update_entries_batch,
            commands::preview::get_asset_preview_manifest,
            commands::preview::get_asset_bytes,
            commands::preview::get_mod_icon,
            commands::preview::load_thumbnail,
            commands::preview::save_thumbnail,
            commands::preview::detect_minecraft_jars,
            commands::preview::set_minecraft_jar,
            commands::preview::get_minecraft_jar_setting,
            commands::export::validate_project,
            commands::export::export_language_file,
            commands::export::export_resource_pack,
            commands::export::export_modified_jar,
            commands::export::get_export_overview,
            commands::export::validate_selected_project,
            commands::export::export_selected_merged_resource_pack,
            commands::export::export_selected_modified_jars,
            commands::export::export_modpack_files,
            commands::export::export_shaderpacks,
            commands::export::restore_modpack_file,
            commands::export::export_selected_language_files,
            commands::export::restore_modified_jar,
            commands::export::open_output_folder,
            commands::settings::get_settings,
            commands::settings::save_settings,
            commands::settings::clear_cache,
            commands::logs::log_frontend_error,
            commands::logs::get_recent_log,
            commands::logs::open_log_folder,
            commands::updater::automatic_updates_supported,
            commands::ai::get_ai_settings,
            commands::ai::save_ai_provider,
            commands::ai::delete_ai_provider,
            commands::ai::test_ai_provider,
            commands::ai::list_ai_models,
            commands::ai::start_ai_job,
            commands::ai::cancel_ai_job,
            commands::ai::start_chatgpt_login,
            commands::ai::cancel_chatgpt_login,
            commands::ai::logout_chatgpt,
            commands::glossary::list_glossary_terms,
            commands::glossary::upsert_glossary_term,
            commands::glossary::upsert_glossary_terms,
            commands::glossary::delete_glossary_term,
            commands::glossary::import_glossary,
            commands::glossary::export_glossary,
            commands::glossary::glossary_candidates,
        ])
        .build(tauri::generate_context!())
        .expect("YakuCraft を起動できませんでした");

    app.run(|handle, event| {
        if matches!(event, tauri::RunEvent::Exit)
            && let Some(state) = handle.try_state::<state::AppState>()
        {
            state.flush_and_close_project();
        }
    });
}
