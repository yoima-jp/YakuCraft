use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

use crate::{error::AppError, logging, state::AppState};

const MAX_LOG_BYTES: u64 = 256 * 1024;
const MAX_LOG_LINES: usize = 500;
const MAX_FRONTEND_ERROR_BYTES: usize = 4 * 1024;

#[tauri::command]
pub fn log_frontend_error(error: String) {
    let error = logging::truncate_utf8(&error, MAX_FRONTEND_ERROR_BYTES);
    let error = logging::sanitize_sensitive_lines(error);
    tracing::warn!(%error, "Frontend error");
}

#[tauri::command]
pub fn get_recent_log(state: State<'_, AppState>) -> Result<String, AppError> {
    read_recent_log(&state.paths.logs_dir).map_err(|error| {
        AppError::new(
            "log_read_failed",
            "ログを読み込めませんでした。",
            Some(error.to_string()),
        )
    })
}

#[tauri::command]
pub fn open_log_folder(app: AppHandle, state: State<'_, AppState>) -> Result<(), AppError> {
    app.opener()
        .open_path(
            state.paths.logs_dir.to_string_lossy().into_owned(),
            None::<&str>,
        )
        .map_err(|error| {
            AppError::new(
                "log_folder_open_failed",
                "ログフォルダーを開けませんでした。",
                Some(error.to_string()),
            )
        })
}

fn read_recent_log(logs_dir: &Path) -> io::Result<String> {
    let Some(path) = latest_log_path(logs_dir)? else {
        return Ok(String::new());
    };

    let mut file = File::open(path)?;
    let file_size = file.metadata()?.len();
    let start = file_size.saturating_sub(MAX_LOG_BYTES);
    file.seek(SeekFrom::Start(start))?;

    let mut bytes = Vec::with_capacity((file_size - start) as usize);
    file.read_to_end(&mut bytes)?;
    let mut content = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0
        && let Some(line_end) = content.find('\n')
    {
        content.drain(..=line_end);
    }

    let lines = content.lines().collect::<Vec<_>>();
    let first_line = lines.len().saturating_sub(MAX_LOG_LINES);
    Ok(lines[first_line..].join("\n"))
}

fn latest_log_path(logs_dir: &Path) -> io::Result<Option<PathBuf>> {
    let mut latest: Option<(String, PathBuf)> = None;
    for entry in fs::read_dir(logs_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }

        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(date) = name.strip_prefix("yakucraft.log.") else {
            continue;
        };
        if !is_log_date(date) {
            continue;
        }

        if latest
            .as_ref()
            .is_none_or(|(latest_date, _)| date > latest_date.as_str())
        {
            latest = Some((date.to_owned(), entry.path()));
        }
    }

    Ok(latest.map(|(_, path)| path))
}

fn is_log_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}
