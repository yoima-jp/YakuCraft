use std::{collections::HashMap, fs, sync::Arc};

use serde::Serialize;
use tauri::State;
use yc_project::{GlossaryTerm, GlossaryTermInput, ProjectEntry};

use crate::{error::AppError, state::AppState};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlossaryCandidate {
    pub source_term: String,
    pub target_term: String,
    pub count: usize,
}

#[tauri::command]
pub async fn list_glossary_terms(
    state: State<'_, AppState>,
) -> Result<Vec<GlossaryTerm>, AppError> {
    let active_project = Arc::clone(&state.active_project);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| project_error("開いているプロジェクトがありません"))?;
        opened
            .repository()
            .list_glossary_terms()
            .map_err(|error| database_error(error.to_string()))
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn upsert_glossary_term(
    term: GlossaryTermInput,
    id: Option<i64>,
    state: State<'_, AppState>,
) -> Result<Vec<GlossaryTerm>, AppError> {
    let Some(id) = id else {
        return upsert_glossary_terms(vec![term], state).await;
    };
    if term.source_term.trim().is_empty() || term.target_term.trim().is_empty() {
        return Err(AppError::simple(
            "invalid_glossary_term",
            "原文と訳語を入力してください。",
        ));
    }

    let active_project = Arc::clone(&state.active_project);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| project_error("開いているプロジェクトがありません"))?;
        let current_terms = opened
            .repository()
            .list_glossary_terms()
            .map_err(|error| database_error(error.to_string()))?;
        // Return a stable conflict code so the editor can mark this row's source field.
        if glossary_source_conflicts(&current_terms, id, &term) {
            return Err(AppError::simple("glossary_term_conflict", "重複しています"));
        }
        opened
            .writer()
            .update_glossary_term(id, term)
            .map_err(|error| database_error(error.to_string()))?;
        opened
            .repository()
            .list_glossary_terms()
            .map_err(|error| database_error(error.to_string()))
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn upsert_glossary_terms(
    terms: Vec<GlossaryTermInput>,
    state: State<'_, AppState>,
) -> Result<Vec<GlossaryTerm>, AppError> {
    if terms
        .iter()
        .any(|term| term.source_term.trim().is_empty() || term.target_term.trim().is_empty())
    {
        return Err(AppError::simple(
            "invalid_glossary_term",
            "原文と訳語を入力してください。",
        ));
    }
    let active_project = Arc::clone(&state.active_project);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| project_error("開いているプロジェクトがありません"))?;
        opened
            .writer()
            .upsert_glossary_terms(terms)
            .map_err(|error| database_error(error.to_string()))?;
        opened
            .repository()
            .list_glossary_terms()
            .map_err(|error| database_error(error.to_string()))
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn delete_glossary_term(
    id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<GlossaryTerm>, AppError> {
    let active_project = Arc::clone(&state.active_project);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| project_error("開いているプロジェクトがありません"))?;
        opened
            .writer()
            .delete_glossary_term(id)
            .map_err(|error| database_error(error.to_string()))?;
        opened
            .repository()
            .list_glossary_terms()
            .map_err(|error| database_error(error.to_string()))
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn import_glossary(
    path: String,
    state: State<'_, AppState>,
) -> Result<Vec<GlossaryTerm>, AppError> {
    let contents = fs::read_to_string(&path).map_err(|error| {
        AppError::new(
            "glossary_import",
            "用語集を読み込めませんでした。",
            Some(error.to_string()),
        )
    })?;
    let terms = yc_project::parse_glossary_import(&contents).map_err(|error| {
        AppError::new(
            "glossary_import",
            "CSVまたはTSVの内容を確認してください。",
            Some(error.to_string()),
        )
    })?;
    upsert_glossary_terms(terms, state).await
}

#[tauri::command]
pub async fn export_glossary(path: String, state: State<'_, AppState>) -> Result<usize, AppError> {
    let terms = list_glossary_terms(state).await?;
    let contents = yc_project::format_glossary_export(&terms)
        .map_err(|error| database_error(error.to_string()))?;
    fs::write(&path, contents).map_err(|error| {
        AppError::new(
            "glossary_export",
            "用語集を書き出せませんでした。",
            Some(error.to_string()),
        )
    })?;
    Ok(terms.len())
}

#[tauri::command]
pub async fn glossary_candidates(
    state: State<'_, AppState>,
) -> Result<Vec<GlossaryCandidate>, AppError> {
    let active_project = Arc::clone(&state.active_project);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| project_error("開いているプロジェクトがありません"))?;
        let terms = opened
            .repository()
            .list_glossary_terms()
            .map_err(|error| database_error(error.to_string()))?;
        let entries = opened
            .repository()
            .list_entries(None)
            .map_err(|error| database_error(error.to_string()))?;
        Ok(collect_candidates(&entries, &terms))
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

fn collect_candidates(entries: &[ProjectEntry], terms: &[GlossaryTerm]) -> Vec<GlossaryCandidate> {
    let existing = terms
        .iter()
        .map(|term| term.source_term.to_lowercase())
        .collect::<std::collections::HashSet<_>>();
    let mut grouped: HashMap<(String, String), GlossaryCandidate> = HashMap::new();
    for entry in entries {
        if !["item.", "block.", "entity."]
            .iter()
            .any(|prefix| entry.key.starts_with(prefix))
        {
            continue;
        }
        let Some(source_term) = entry.source_text.as_deref().map(str::trim) else {
            continue;
        };
        let Some(target_term) = entry.target_text.as_deref().map(str::trim) else {
            continue;
        };
        if source_term.is_empty()
            || target_term.is_empty()
            || source_term.split_whitespace().count() > 4
            || existing.contains(&source_term.to_lowercase())
        {
            continue;
        }
        let key = (source_term.to_lowercase(), target_term.to_owned());
        grouped
            .entry(key)
            .and_modify(|candidate| candidate.count += 1)
            .or_insert_with(|| GlossaryCandidate {
                source_term: source_term.to_owned(),
                target_term: target_term.to_owned(),
                count: 1,
            });
    }
    let mut candidates = grouped.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.source_term
            .to_lowercase()
            .cmp(&right.source_term.to_lowercase())
            .then_with(|| left.target_term.cmp(&right.target_term))
    });
    candidates
}

fn glossary_source_conflicts(terms: &[GlossaryTerm], id: i64, term: &GlossaryTermInput) -> bool {
    let source_term = term.source_term.trim();
    terms.iter().any(|existing| {
        existing.id != id
            && (existing.source_term == source_term
                || (!term.case_sensitive
                    && !existing.case_sensitive
                    && existing.source_term.eq_ignore_ascii_case(source_term)))
    })
}

fn project_error(detail: &str) -> AppError {
    AppError::new("project_required", detail, None)
}

fn database_error(detail: impl Into<String>) -> AppError {
    AppError::new(
        "database",
        "用語集を保存できませんでした。もう一度お試しください。",
        Some(detail.into()),
    )
}

fn state_error(detail: &str) -> AppError {
    AppError::new(
        "internal_state",
        "アプリの状態を読み取れませんでした。再起動してもう一度お試しください。",
        Some(detail.to_owned()),
    )
}

fn task_error(detail: String) -> AppError {
    AppError::new(
        "task_failed",
        "処理に失敗しました。もう一度お試しください。",
        Some(detail),
    )
}
