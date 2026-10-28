use std::sync::Arc;

use serde::Serialize;
use tauri::State;
use yc_language::{EntryFlag, TranslationOrigin};
use yc_project::{
    DocumentProjectEntry, EntryUpdate, OpenedProject, ProjectEntry, TranslationEntryKind,
};

use crate::{commands::project::refresh_recent_project_progress, error::AppError, state::AppState};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorEntry {
    id: i64,
    mod_unit_id: i64,
    entry_kind: TranslationEntryKind,
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
            entry_kind: TranslationEntryKind::Lang,
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

impl From<DocumentProjectEntry> for EditorEntry {
    fn from(entry: DocumentProjectEntry) -> Self {
        let key = match entry.entry_kind {
            TranslationEntryKind::Ftbquests => ftbquests_display_key(&entry.document_pointer),
            _ => document_display_key(&entry.document_path, &entry.document_pointer),
        };
        Self {
            id: entry.id,
            mod_unit_id: entry.mod_unit_id,
            entry_kind: entry.entry_kind,
            namespace: entry.namespace,
            key,
            source_text: entry.source_text,
            source_locale: entry.source_locale,
            target_text: entry.target_text,
            origin: entry.origin,
            flags: entry.flags,
        }
    }
}

fn ftbquests_display_key(pointer: &str) -> String {
    let field_pointer = pointer.split('[').next().unwrap_or(pointer);
    let parts: Vec<_> = field_pointer.split('.').collect();
    if parts.len() < 3 || parts[0] != "quest" {
        return pointer.to_owned();
    }
    let field = match parts[2] {
        "title" => "タイトル".to_owned(),
        "quest_desc" => pointer
            .rsplit_once('[')
            .and_then(|(_, index)| index.strip_suffix(']'))
            .and_then(|index| index.parse::<usize>().ok())
            .map(|index| format!("説明 {}行目", index + 1))
            .unwrap_or_else(|| "説明".to_owned()),
        _ => parts[2..].join("."),
    };
    format!("quest.{} › {field}", parts[1])
}

fn document_display_key(document_path: &str, pointer: &str) -> String {
    let document_path = document_path
        .split_once("!/")
        .map_or(document_path, |(_, inner)| inner);
    let parts: Vec<_> = document_path.split('/').collect();
    let (book, relative_path) = parts
        .iter()
        .position(|part| *part == "patchouli_books")
        .filter(|index| parts.len() > index + 3)
        .map(|index| (parts[index + 1], parts[index + 3..].join("/")))
        .unwrap_or(("Patchouli", document_path.to_owned()));
    let field = pointer
        .split('/')
        .skip(1)
        .map(|part| part.replace("~1", "/").replace("~0", "~"))
        .fold(String::new(), |mut output, part| {
            if part.parse::<usize>().is_ok() {
                output.push_str(&format!("[{part}]"));
            } else {
                if !output.is_empty() {
                    output.push('.');
                }
                output.push_str(&part);
            }
            output
        });
    format!("{book} › {relative_path} › {field}")
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
        let repository = opened.repository();
        let mut entries: Vec<_> = repository
            .list_entries(mod_unit_id)
            .map_err(|error| database_error(error.to_string()))?
            .into_iter()
            .map(EditorEntry::from)
            .collect();
        entries.extend(
            repository
                .list_document_entries(mod_unit_id)
                .map_err(|error| database_error(error.to_string()))?
                .into_iter()
                .map(EditorEntry::from),
        );
        entries.sort_by(|left, right| {
            (left.mod_unit_id, &left.namespace, left.id).cmp(&(
                right.mod_unit_id,
                &right.namespace,
                right.id,
            ))
        });
        Ok(entries)
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
        let updated_ids: std::collections::HashSet<_> =
            updates.iter().map(|update| update.id).collect();
        update_entries_batch_core(opened, updates)?;
        let repository = opened.repository();
        let mut result: Vec<_> = repository
            .list_entries(None)
            .map_err(|error| database_error(error.to_string()))?
            .into_iter()
            .filter(|entry| updated_ids.contains(&entry.id))
            .map(EditorEntry::from)
            .collect();
        result.extend(
            repository
                .list_document_entries(None)
                .map_err(|error| database_error(error.to_string()))?
                .into_iter()
                .filter(|entry| updated_ids.contains(&entry.id))
                .map(EditorEntry::from),
        );
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
    let before_documents = opened
        .repository()
        .list_document_entries(None)
        .map_err(|error| database_error(error.to_string()))?;
    let shaderpack_unit_ids: std::collections::HashSet<_> = opened
        .repository()
        .list_mod_units()
        .map_err(|error| database_error(error.to_string()))?
        .into_iter()
        .filter(|unit| unit.source_kind == yc_project::ModUnitSourceKind::Shaderpack)
        .map(|unit| unit.id)
        .collect();
    let entries_by_id: std::collections::HashMap<_, _> = before
        .iter()
        .map(|entry| {
            (
                entry.id,
                (
                    entry.source_text.as_deref().unwrap_or_default(),
                    &entry.flags,
                    if entry.flags.contains(&EntryFlag::PatchouliText) {
                        yc_validation::PlaceholderProfile::Patchouli
                    } else {
                        yc_validation::PlaceholderProfile::Lang
                    },
                    shaderpack_unit_ids.contains(&entry.mod_unit_id),
                ),
            )
        })
        .chain(before_documents.iter().map(|entry| {
            (
                entry.id,
                (
                    entry.source_text.as_deref().unwrap_or_default(),
                    &entry.flags,
                    match entry.entry_kind {
                        TranslationEntryKind::Patchouli => {
                            yc_validation::PlaceholderProfile::Patchouli
                        }
                        TranslationEntryKind::Ftbquests => {
                            yc_validation::PlaceholderProfile::FtbQuests
                        }
                        TranslationEntryKind::Lang => yc_validation::PlaceholderProfile::Lang,
                    },
                    false,
                ),
            )
        }))
        .collect();
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
            let (source_text, flags, profile, is_shaderpack) = entries_by_id[&update.id];
            (
                update.id,
                if is_shaderpack {
                    !update.target_text.trim().is_empty()
                        && yc_validation::shader_format_warning(source_text, &update.target_text)
                } else {
                    yc_validation::validate_manual_edit_with_profile(
                        source_text,
                        &update.target_text,
                        profile,
                    )
                    .placeholder_error
                },
                flags.contains(&EntryFlag::PlaceholderError),
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
