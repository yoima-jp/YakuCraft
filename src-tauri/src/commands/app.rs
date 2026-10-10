use serde::Serialize;
use tauri::{AppHandle, State};

use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub app_name: String,
    pub version: String,
    pub data_directory: String,
}

#[tauri::command]
pub fn get_app_info(app: AppHandle, state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        app_name: app
            .config()
            .product_name
            .clone()
            .unwrap_or_else(|| app.package_info().name.clone()),
        version: app.package_info().version.to_string(),
        data_directory: state.paths.data_dir.to_string_lossy().into_owned(),
    }
}

#[tauri::command]
pub fn exit_app(app: AppHandle) -> Result<(), AppError> {
    // Autosave完了後の終了はRust側で行い、フロントにウィンドウ終了権限を追加しない。
    app.exit(0);
    Ok(())
}
