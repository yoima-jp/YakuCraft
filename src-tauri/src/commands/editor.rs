use std::sync::Arc;

use serde::Serialize;
use tauri::State;
use yc_language::{EntryFlag, TranslationOrigin};
use yc_project::{EntryUpdate, OpenedProject, ProjectEntry};

use crate::{commands::project::refresh_recent_project_progress, error::AppError, state::AppState};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorEntry {
    id: i64,
    mod_unit_id: i64,
    namespace: String,
    key: String,
    source_text: Option<String>,
    source_locale: Option<String>,
    target_text: Option<String>,
    origin: TranslationOrigin,
    flags: Vec<EntryFlag>,
}

impl From<ProjectEntry> for EditorEntry {
    fn from(entry: ProjectEntry) -> Self {
        Self {
            id: entry.id,
            mod_unit_id: entry.mod_unit_id,
            namespace: entry.namespace,
            key: entry.key,
            source_text: entry.source_text,
            source_locale: entry.source_locale,
            target_text: entry.target_text,
            origin: entry.origin,
            flags: entry.flags,
        }
    }
}

#[tauri::command]
pub async fn list_entries(
    mod_unit_id: Option<i64>,
    state: State<'_, AppState>,
) -> Result<Vec<EditorEntry>, AppError> {
    let active_project = Arc::clone(&state.active_project);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| database_error("開いているプロジェクトがありません"))?;
        opened
            .repository()
            .list_entries(mod_unit_id)
            .map(|entries| entries.into_iter().map(EditorEntry::from).collect())
            .map_err(|error| database_error(error.to_string()))
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn update_entries_batch(
    updates: Vec<EntryUpdate>,
    state: State<'_, AppState>,
) -> Result<Vec<EditorEntry>, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let paths = state.paths.clone();
    let recent_lock = Arc::clone(&state.recent_lock);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| database_error("開いているプロジェクトがありません"))?;
        let result = update_entries_batch_core(opened, updates)?
            .into_iter()
            .map(EditorEntry::from)
            .collect();
        match recent_lock.lock() {
            Ok(_guard) => {
                if let Err(error) = refresh_recent_project_progress(&paths, opened.summary()) {
                    tracing::warn!(%error.user_message, "recent translation progress could not be updated");
                }
            }
            Err(_) => tracing::warn!("recent projects lock was poisoned"),
        }
        Ok(result)
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

pub(crate) fn update_entries_batch_core(
    opened: &OpenedProject,
    updates: Vec<EntryUpdate>,
) -> Result<Vec<ProjectEntry>, AppError> {
    if updates.is_empty() {
        return Ok(Vec::new());
    }
    let before = opened
        .repository()
        .list_entries(None)
        .map_err(|error| database_error(error.to_string()))?;
    let entries_by_id: std::collections::HashMap<_, _> =
        before.iter().map(|entry| (entry.id, entry)).collect();
    let mut seen_ids = std::collections::HashSet::with_capacity(updates.len());
    for update in &updates {
        if !seen_ids.insert(update.id) {
            return Err(AppError::simple(
                "invalid_update_batch",
                "翻訳データを保存できませんでした。もう一度お試しください。",
            ));
        }
        if !entries_by_id.contains_key(&update.id) {
            return Err(database_error(format!(
                "translation entry {} was not found",
                update.id
            )));
        }
    }

    let validations: Vec<_> = updates
        .iter()
        .map(|update| {
            let entry = entries_by_id[&update.id];
            (
                update.id,
                yc_validation::validate_manual_edit(
                    entry.source_text.as_deref().unwrap_or_default(),
                    &update.target_text,
                )
                .placeholder_error,
                entry.flags.contains(&EntryFlag::PlaceholderError),
            )
        })
        .collect();

    //本文の更新を先にwriterへ送り、検証flagは同じwriter queueで順番を保って反映する。
    opened
        .writer()
        .update_entries(updates.clone())
        .map_err(|error| database_error(error.to_string()))?;
    for (entry_id, has_mismatch, had_flag) in validations {
        if has_mismatch && !had_flag {
            opened
                .writer()
                .add_flag(entry_id, EntryFlag::PlaceholderError)
                .map_err(|error| database_error(error.to_string()))?;
        } else if !has_mismatch && had_flag {
            opened
                .writer()
                .remove_flag(entry_id, EntryFlag::PlaceholderError)
                .map_err(|error| database_error(error.to_string()))?;
        }
    }

    let updated_ids: std::collections::HashSet<_> =
        updates.iter().map(|update| update.id).collect();
    Ok(opened
        .repository()
        .list_entries(None)
        .map_err(|error| database_error(error.to_string()))?
        .into_iter()
        .filter(|entry| updated_ids.contains(&entry.id))
        .collect())
}

fn database_error(detail: impl Into<String>) -> AppError {
    AppError::new(
        "database",
        "翻訳データを保存できませんでした。もう一度お試しください。",
        Some(detail.into()),
    )
}

fn task_error(detail: String) -> AppError {
    AppError::new(
        "task_failed",
        "処理に失敗しました。もう一度お試しください。",
        Some(detail),
    )
}

fn state_error(detail: &str) -> AppError {
    AppError::new(
        "internal_state",
        "アプリの状態を読み取れませんでした。再起動してもう一度お試しください。",
        Some(detail.to_owned()),
    )
}
