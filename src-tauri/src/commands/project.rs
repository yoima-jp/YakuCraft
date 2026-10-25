use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, MutexGuard},
    time::Instant,
};

use serde::{Deserialize, Serialize};
use tauri::{State, ipc::Channel};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;
use yc_analysis::{
    AnalysisData, AnalysisProgress, CancellationToken, ModScanResult, ScanOptions, ScanProgress,
    scan_mods_folder as scan_mods_folder_core,
};
use yc_assets::{AssetPack, AssetResolver, AssetSource, AssetType, LinkConfidence, SourceKind};
use yc_language::{
    DiscoveredLanguageFile, EntryFlag, LanguageFileFormat, ReviewSettings, TranslationEntry,
    TranslationOrigin, normalize_locale,
};
use yc_project::{
    AnalysisData as ProjectAnalysisData, AssetRecord, DocumentEntryInput, IncludedNamespace,
    LanguageRecord, ModRecord, ModUnitRecord, ModUnitSourceKind, ModpackSource, NamespaceRecord,
    NestedArchiveRecord, OpenedProject, ProjectAssetIndex, ProjectEntry, ProjectEntrySummary,
    ProjectReviewSettings, ProjectSummary, RecentProject, RecentProjects, TranslationAssetMatch,
    TranslationProgress, create_project_with_modpack, delete_project as remove_project,
    list_projects, load_recent_projects, load_settings, normalize_project_name,
    open_project as open_project_file, rename_project as rename_project_file, save_recent_projects,
};
use yc_validation::{PlaceholderProfile, compare_placeholders_for_profile};

const PROJECT_ASSET_SOURCE_ID: &str = "project-source";

use crate::{
    error::AppError,
    paths::AppPaths,
    state::{AppState, CachedProjectAnalysis, CachedProjectAssetIndex, CachedUnitAssetIndex},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSettingsInput {
    pub source_locale: String,
    pub fallback_priority: Vec<String>,
    pub preserve_existing_japanese: bool,
    pub fill_missing_from_other_languages: bool,
    /// Namespace名はunit間で重複するため、選択をunit IDと組にして保存する。
    pub included_namespaces: Vec<IncludedNamespace>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModSummary {
    pub mod_unit_id: i64,
    pub mod_id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub loader: String,
    pub minecraft_version: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamespaceSummary {
    pub mod_unit_id: i64,
    pub namespace: String,
    pub container_path: String,
    pub included: bool,
    pub entry_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageSummary {
    pub mod_unit_id: i64,
    pub namespace: String,
    pub locale: String,
    pub format: String,
    pub entry_count: usize,
    pub container_path: String,
    pub archive_path: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedArchiveSummary {
    pub mod_unit_id: i64,
    pub archive_path: String,
    pub mod_id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub has_languages: bool,
    pub skipped_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCounts {
    pub total: usize,
    pub translated: usize,
    pub missing: usize,
    pub needs_review: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAssetSummary {
    pub total: usize,
    pub block_models: usize,
    pub item_models: usize,
    pub entity_textures: usize,
    pub geometry_models: usize,
    pub unresolved_entries: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetails {
    pub project: ProjectSummary,
    pub mods: Vec<ModSummary>,
    pub namespaces: Vec<NamespaceSummary>,
    pub languages: Vec<LanguageSummary>,
    pub nested_archives: Vec<NestedArchiveSummary>,
    pub recommended_source_locale: Option<String>,
    pub source_locale: String,
    pub fallback_priority: Vec<String>,
    pub preserve_existing_japanese: bool,
    pub fill_missing_from_other_languages: bool,
    pub included_namespaces: Vec<IncludedNamespace>,
    pub setup_completed: bool,
    pub existing_japanese_translation_count: usize,
    pub missing_source_keys_count: usize,
    pub counts: ProjectCounts,
    pub target_language_file_name: String,
    pub target_language_format: String,
    pub is_signed: bool,
    pub asset_count: Option<usize>,
    pub asset_summary: ProjectAssetSummary,
    pub minecraft_version_detection: Option<yc_export::MinecraftVersionDetection>,
    pub supported_minecraft_versions: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSetupSummary {
    pub existing_japanese_translation_count: usize,
    pub missing_source_keys_count: usize,
    pub assets: ProjectAssetSummary,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisCompleted {
    pub job_id: String,
    pub project_id: String,
    pub failed_mods: Vec<ModImportFailure>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModImportFailure {
    pub filename: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModUnitSummary {
    pub id: i64,
    pub source_kind: ModUnitSourceKind,
    pub source_filename: String,
    pub mod_id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub loader: Option<String>,
    pub minecraft_version: Option<String>,
    pub total: usize,
    pub translated: usize,
    pub needs_review: usize,
    pub ftb_quest_entries: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModUnitRemovalInfo {
    pub has_unrestored_replacements: bool,
    pub has_unrestored_file_deployments: bool,
}

pub(crate) struct CreatedProject {
    pub opened: OpenedProject,
    pub mod_version: Option<String>,
    pub analyses: HashMap<i64, Arc<AnalysisData>>,
    pub failed_mods: Vec<ModImportFailure>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationProgress {
    pub percent: Option<u8>,
    pub message: String,
}

fn send_operation_progress(
    channel: &Channel<OperationProgress>,
    percent: Option<u8>,
    message: &str,
) {
    if let Err(error) = channel.send(OperationProgress {
        percent,
        message: message.to_owned(),
    }) {
        tracing::debug!(%error, "operation progress channel closed");
    }
}

fn cached_project_analysis(
    cache: &Arc<std::sync::Mutex<Option<CachedProjectAnalysis>>>,
    project_id: &str,
    mod_unit_id: i64,
) -> Result<Option<Arc<AnalysisData>>, AppError> {
    let cached = cache
        .lock()
        .map_err(|_| internal_state_error("Project analysis cache"))?;
    Ok(cached
        .as_ref()
        .filter(|cached| cached.project_id == project_id)
        .and_then(|cached| cached.analyses.get(&mod_unit_id))
        .map(Arc::clone))
}

fn take_cached_project_asset_index(
    cache: &Arc<std::sync::Mutex<Option<CachedProjectAssetIndex>>>,
    project_id: &str,
    mod_unit_id: i64,
    included_namespaces: &[String],
) -> Result<Option<ProjectAssetIndex>, AppError> {
    let mut cache = cache
        .lock()
        .map_err(|_| internal_state_error("Project asset index cache"))?;
    let selected_namespaces: BTreeSet<_> = included_namespaces.iter().cloned().collect();
    if cache
        .as_ref()
        .is_some_and(|cached| cached.project_id == project_id)
        && let Some(unit_cache) = cache
            .as_mut()
            .and_then(|cached| cached.indices.get(&mod_unit_id))
    {
        if unit_cache.included_namespaces == selected_namespaces {
            return Ok(cache
                .as_mut()
                .and_then(|cached| cached.indices.remove(&mod_unit_id))
                .map(|cached| cached.index));
        }
        if let Some(cached) = cache.as_mut() {
            cached.indices.remove(&mod_unit_id);
        }
    }
    // 選択が変わったunitだけ破棄し、他unitの解析結果と索引を保つ。
    Ok(None)
}

fn review_asset_index_matches(index: &ProjectAssetIndex, entries: &[TranslationEntry]) -> bool {
    let expected_matches: BTreeSet<_> = entries
        .iter()
        .filter(|entry| is_asset_translation_key(&entry.key))
        .map(|entry| (entry.namespace.clone(), entry.key.clone()))
        .collect();
    let cached_matches: BTreeSet<_> = index
        .matches
        .iter()
        .map(|entry| (entry.namespace.clone(), entry.key.clone()))
        .collect();
    expected_matches == cached_matches
}

fn cache_project_analysis(
    cache: &Arc<std::sync::Mutex<Option<CachedProjectAnalysis>>>,
    project_id: &str,
    mod_unit_id: i64,
    analysis: Arc<AnalysisData>,
) -> Result<(), AppError> {
    let mut cache = cache
        .lock()
        .map_err(|_| internal_state_error("Project analysis cache"))?;
    if cache
        .as_ref()
        .is_none_or(|cached| cached.project_id != project_id)
    {
        *cache = Some(CachedProjectAnalysis {
            project_id: project_id.to_owned(),
            analyses: HashMap::new(),
        });
    }
    if let Some(cached) = cache.as_mut() {
        cached.analyses.insert(mod_unit_id, analysis);
    }
    Ok(())
}

fn cache_project_asset_index(
    cache: &Arc<std::sync::Mutex<Option<CachedProjectAssetIndex>>>,
    project_id: &str,
    mod_unit_id: i64,
    included_namespaces: &[String],
    index: ProjectAssetIndex,
) -> Result<(), AppError> {
    let mut cache = cache
        .lock()
        .map_err(|_| internal_state_error("Project asset index cache"))?;
    if cache
        .as_ref()
        .is_none_or(|cached| cached.project_id != project_id)
    {
        *cache = Some(CachedProjectAssetIndex {
            project_id: project_id.to_owned(),
            indices: HashMap::new(),
        });
    }
    if let Some(cached) = cache.as_mut() {
        cached.indices.insert(
            mod_unit_id,
            CachedUnitAssetIndex {
                included_namespaces: included_namespaces.iter().cloned().collect(),
                index,
            },
        );
    }
    Ok(())
}

fn analysis_progress_percent(progress: &AnalysisProgress) -> Option<u8> {
    let stage_index = match progress.stage {
        yc_analysis::AnalysisStage::ValidateJar => 0,
        yc_analysis::AnalysisStage::Sha256 => 1,
        yc_analysis::AnalysisStage::ScanZipPaths => 2,
        yc_analysis::AnalysisStage::ParseMetadata => 3,
        yc_analysis::AnalysisStage::ScanNestedJars => 4,
        yc_analysis::AnalysisStage::DiscoverNamespaces => 5,
        yc_analysis::AnalysisStage::ParseLanguages => 6,
        yc_analysis::AnalysisStage::DetectTargetTranslation => 7,
    };
    let total = progress.total.filter(|total| *total > 0)?;
    let fraction = progress.current.min(total) as f64 / total as f64;
    Some(5 + (((stage_index as f64 + fraction) / 8.0) * 55.0) as u8)
}

fn load_project_analysis(
    opened: &OpenedProject,
    mod_unit_id: i64,
    cache: &Arc<std::sync::Mutex<Option<CachedProjectAnalysis>>>,
    on_progress: impl FnMut(AnalysisProgress),
) -> Result<Arc<AnalysisData>, AppError> {
    let project_id = opened.summary().id.as_str();
    if let Some(analysis) = cached_project_analysis(cache, project_id, mod_unit_id)? {
        return Ok(analysis);
    }
    // Reviewの選択変更ではJAR自体は変わらない。同じ内容を毎回解析すると、集計と保存の待ち時間が重複する。
    let source_path = opened
        .repository()
        .mod_unit_source_path(mod_unit_id)
        .map_err(|error| database_error(error.to_string()))?;
    let source_kind = opened
        .repository()
        .list_mod_units()
        .map_err(|error| database_error(error.to_string()))?
        .into_iter()
        .find(|unit| unit.id == mod_unit_id)
        .map(|unit| unit.source_kind)
        .ok_or_else(|| database_error("modが見つかりません".to_owned()))?;
    let analysis = Arc::new(match source_kind {
        ModUnitSourceKind::Jar => {
            yc_analysis::analyze_jar(&source_path, &CancellationToken::default(), on_progress)
                .map_err(analysis_error)?
        }
        ModUnitSourceKind::Modpack => super::modpack::analyze_modpack_archive(&source_path)?.0,
    });
    cache_project_analysis(cache, project_id, mod_unit_id, Arc::clone(&analysis))?;
    Ok(analysis)
}

fn first_mod_unit_id(opened: &OpenedProject) -> Result<i64, AppError> {
    opened
        .repository()
        .list_mod_units()
        .map_err(|error| database_error(error.to_string()))?
        .first()
        .map(|unit| unit.id)
        .ok_or_else(|| database_error("プロジェクトにmodがありません".to_owned()))
}

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AnalysisChannelEvent {
    Progress {
        // 遅れて届いた進捗で別ジョブの状態を更新しないよう、終端通知と同じIDを送る。
        job_id: String,
        progress: AnalysisProgress,
        mod_index: usize,
        mod_count: usize,
        mod_filename: String,
    },
    Completed {
        completed: AnalysisCompleted,
    },
    Failed {
        job_id: String,
        error: AppError,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ScanModsFolderEvent {
    Progress {
        progress: ScanProgress,
    },
    Completed {
        job_id: String,
        results: Vec<ModScanResult>,
        scanned_folder: PathBuf,
        instance_dir: Option<PathBuf>,
        looks_like_instance_dir: bool,
        modpack_scan: Option<yc_modpack::ModpackScan>,
    },
    Failed {
        job_id: String,
        error: AppError,
    },
}

struct ResolvedModsFolder {
    scanned_folder: PathBuf,
    instance_dir: Option<PathBuf>,
}

fn resolve_mods_folder(folder: &Path) -> Result<ResolvedModsFolder, std::io::Error> {
    let mut has_direct_jar = false;
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
            && fs::metadata(&path).is_ok_and(|metadata| metadata.is_file())
        {
            has_direct_jar = true;
            break;
        }
    }

    let nested_mods_folder = folder.join("mods");
    // インスタンス本体を選んだ場合は直下JARがなければmods/を走査し、選択元のパスを保持する。
    if !has_direct_jar && nested_mods_folder.is_dir() {
        return Ok(ResolvedModsFolder {
            scanned_folder: nested_mods_folder,
            instance_dir: Some(folder.to_path_buf()),
        });
    }

    Ok(ResolvedModsFolder {
        scanned_folder: folder.to_path_buf(),
        instance_dir: yc_export::instance_dir_from_mods_dir(folder),
    })
}

#[tauri::command]
pub fn cancel_analysis(job_id: String, state: State<'_, AppState>) -> Result<bool, AppError> {
    let jobs = state
        .analysis_jobs
        .lock()
        .map_err(|_| internal_state_error("analysis job state"))?;
    if let Some(token) = jobs.get(&job_id) {
        token.cancel();
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tauri::command]
pub fn scan_mods_folder(
    path: String,
    channel: Channel<ScanModsFolderEvent>,
    state: State<'_, AppState>,
) -> Result<String, AppError> {
    let job_id = Uuid::new_v4().to_string();
    let cancellation = CancellationToken::default();
    state
        .analysis_jobs
        .lock()
        .map_err(|_| internal_state_error("analysis job state"))?
        .insert(job_id.clone(), cancellation.clone());

    let analysis_jobs = Arc::clone(&state.analysis_jobs);
    let task_job_id = job_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let folder = PathBuf::from(path);
        let result = resolve_mods_folder(&folder)
            .map_err(|source| yc_analysis::AnalysisError::ScanFolderIo {
                path: folder.clone(),
                source,
            })
            .and_then(|resolved| {
                scan_mods_folder_core(
                    &resolved.scanned_folder,
                    &ScanOptions::default(),
                    &cancellation,
                    |progress| {
                        if let Err(error) = channel.send(ScanModsFolderEvent::Progress { progress })
                        {
                            tracing::debug!(%error, "mod folder scan progress channel closed");
                        }
                    },
                )
                .map(|results| {
                    let candidate = resolved
                        .instance_dir
                        .clone()
                        .unwrap_or_else(|| folder.clone());
                    let scan = yc_modpack::scan_instance(&candidate);
                    let modpack_scan = (!scan.files.is_empty()
                        || scan.ftb_quests != yc_modpack::FtbQuestsStatus::None)
                        .then_some(scan);
                    let instance_dir = resolved
                        .instance_dir
                        .or_else(|| modpack_scan.as_ref().map(|_| candidate));
                    (resolved.scanned_folder, instance_dir, results, modpack_scan)
                })
            });

        let event = match result {
            Ok((scanned_folder, instance_dir, results, modpack_scan)) => {
                let looks_like_instance_dir = instance_dir
                    .as_deref()
                    .is_some_and(yc_export::looks_like_instance_dir)
                    || modpack_scan.is_some();
                ScanModsFolderEvent::Completed {
                    job_id: task_job_id.clone(),
                    results,
                    scanned_folder,
                    instance_dir,
                    looks_like_instance_dir,
                    modpack_scan,
                }
            }
            Err(error) => {
                let detail = error.to_string();
                let error = match error {
                    yc_analysis::AnalysisError::Cancelled => cancelled_error(),
                    yc_analysis::AnalysisError::ScanFolderIo { .. } => AppError::new(
                        "mods_folder_read",
                        "フォルダを読み込めませんでした。選択したフォルダを確認してください。",
                        Some(detail),
                    ),
                    other => analysis_error(other),
                };
                ScanModsFolderEvent::Failed {
                    job_id: task_job_id.clone(),
                    error,
                }
            }
        };

        if let Err(error) = channel.send(event) {
            tracing::debug!(%error, "mod folder scan result channel closed");
        }
        if let Ok(mut jobs) = analysis_jobs.lock() {
            jobs.remove(&task_job_id);
        }
    });

    Ok(job_id)
}

#[tauri::command]
pub fn create_project_from_jars(
    paths: Vec<String>,
    instance_path: Option<String>,
    include_modpack: bool,
    channel: Channel<AnalysisChannelEvent>,
    state: State<'_, AppState>,
) -> Result<String, AppError> {
    if paths.is_empty() && !include_modpack {
        return Err(AppError::simple(
            "no_jar_selected",
            "JARファイルを選んでください。",
        ));
    }

    let job_id = Uuid::new_v4().to_string();
    let cancellation = CancellationToken::default();
    state
        .analysis_jobs
        .lock()
        .map_err(|_| internal_state_error("analysis job state"))?
        .insert(job_id.clone(), cancellation.clone());

    let app_paths = state.paths.clone();
    let active_project = Arc::clone(&state.active_project);
    let project_io_lock = Arc::clone(&state.project_io_lock);
    let project_analysis_cache = Arc::clone(&state.project_analysis_cache);
    let project_asset_index_cache = Arc::clone(&state.project_asset_index_cache);
    let analysis_jobs = Arc::clone(&state.analysis_jobs);
    let recent_lock = Arc::clone(&state.recent_lock);
    let task_job_id = job_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Projectの作成・全unit解析・削除を直列化し、解析中のrenameを防ぐ。
        let _project_io = match project_io_lock.lock() {
            Ok(guard) => guard,
            Err(_) => {
                let _ = channel.send(AnalysisChannelEvent::Failed {
                    job_id: task_job_id.clone(),
                    error: internal_state_error("Project file operations"),
                });
                if let Ok(mut jobs) = analysis_jobs.lock() {
                    jobs.remove(&task_job_id);
                }
                return;
            }
        };
        let source_paths = paths.iter().map(PathBuf::from).collect::<Vec<_>>();
        let instance_path = instance_path.as_deref().map(Path::new);
        let result = create_project_from_sources_core(
            &app_paths,
            &source_paths,
            instance_path,
            include_modpack,
            &cancellation,
            |progress, mod_index, mod_count, mod_filename| {
                if let Err(error) = channel.send(AnalysisChannelEvent::Progress {
                    job_id: task_job_id.clone(),
                    progress,
                    mod_index,
                    mod_count,
                    mod_filename,
                }) {
                    tracing::debug!(%error, "analysis progress channel closed");
                }
            },
        );

        let event = match result {
            Ok(created) => {
                let CreatedProject {
                    opened,
                    mod_version,
                    analyses,
                    failed_mods,
                } = created;
                let project_summary = opened.summary().clone();
                let project_id = project_summary.id.clone();
                match active_project.lock() {
                    Ok(mut active) => {
                        if cancellation.is_cancelled() {
                            drop(active);
                            drop(opened);
                            cleanup_project(&app_paths, &project_id);
                            AnalysisChannelEvent::Failed {
                                job_id: task_job_id.clone(),
                                error: cancelled_error(),
                            }
                        } else {
                            drop(active.replace(opened));
                            if let Ok(mut cached) = project_asset_index_cache.lock() {
                                *cached = None;
                            }
                            match project_analysis_cache.lock() {
                                Ok(mut cached) => {
                                    *cached = Some(CachedProjectAnalysis {
                                        project_id: project_id.clone(),
                                        analyses,
                                    });
                                }
                                Err(error) => {
                                    tracing::warn!(%error, "Project analysis could not be cached")
                                }
                            }
                            match recent_lock.lock() {
                                Ok(_guard) => {
                                    if let Err(error) = touch_recent_project(
                                        &app_paths,
                                        &project_summary,
                                        mod_version,
                                    ) {
                                        // Recent記録の失敗で作成済みDBを失敗扱いにすると、再作成時にデータが重複する。
                                        tracing::warn!(project_id, %error.user_message, "recent project record could not be updated");
                                    }
                                }
                                Err(_) => {
                                    tracing::warn!(project_id, "recent projects lock was poisoned")
                                }
                            }
                            AnalysisChannelEvent::Completed {
                                completed: AnalysisCompleted {
                                    job_id: task_job_id.clone(),
                                    project_id,
                                    failed_mods,
                                },
                            }
                        }
                    }
                    Err(_) => {
                        drop(opened);
                        cleanup_project(&app_paths, &project_id);
                        AnalysisChannelEvent::Failed {
                            job_id: task_job_id.clone(),
                            error: internal_state_error("active Project state"),
                        }
                    }
                }
            }
            Err(error) => AnalysisChannelEvent::Failed {
                job_id: task_job_id.clone(),
                error,
            },
        };

        if let Err(error) = channel.send(event) {
            tracing::debug!(%error, "analysis result channel closed");
        }
        if let Ok(mut jobs) = analysis_jobs.lock() {
            jobs.remove(&task_job_id);
        }
    });

    Ok(job_id)
}

#[tauri::command]
pub async fn open_project(
    project_id: String,
    channel: Channel<OperationProgress>,
    state: State<'_, AppState>,
) -> Result<ProjectDetails, AppError> {
    let id = Uuid::parse_str(&project_id).map_err(|error| {
        AppError::new(
            "project_not_found",
            "プロジェクトを開けませんでした。",
            Some(error.to_string()),
        )
    })?;
    let paths = state.paths.clone();
    let active_project = Arc::clone(&state.active_project);
    let project_io_lock = Arc::clone(&state.project_io_lock);
    let project_analysis_cache = Arc::clone(&state.project_analysis_cache);
    let project_asset_index_cache = Arc::clone(&state.project_asset_index_cache);
    let preview_context = Arc::clone(&state.preview_context);
    let recent_lock = Arc::clone(&state.recent_lock);
    tauri::async_runtime::spawn_blocking(move || {
        send_operation_progress(&channel, Some(0), "プロジェクトを開いています");
        let _project_io = project_io_lock
            .lock()
            .map_err(|_| internal_state_error("Project file operations"))?;
        let mut active = active_project
            .lock()
            .map_err(|_| internal_state_error("active Project state"))?;
        if active
            .as_ref()
            .is_none_or(|opened| opened.summary().id != id.to_string())
        {
            let opened = open_project_file(&paths.projects_dir, id)
                .map_err(|error| database_error(error.to_string()))?;
            *preview_context
                .lock()
                .map_err(|_| internal_state_error("preview cache"))? = None;
            drop(active.replace(opened));
            *project_analysis_cache
                .lock()
                .map_err(|_| internal_state_error("Project analysis cache"))? = None;
            *project_asset_index_cache
                .lock()
                .map_err(|_| internal_state_error("Project asset index cache"))? = None;
        }

        let opened = active
            .as_ref()
            .ok_or_else(|| database_error("Projectを開けませんでした".to_owned()))?;
        let review = opened
            .repository()
            .review_settings()
            .map_err(|error| database_error(error.to_string()))?;
        let details = if review.setup_completed {
            // 完了済みProjectは保存済みのunit別情報を使い、JARを再解析しない。
            send_operation_progress(&channel, Some(70), "保存済み情報を読み込んでいます");
            project_details_from_stored_data(opened, review, None)?
        } else {
            send_operation_progress(&channel, Some(10), "JARを確認しています");
            let details = project_details_for_all_units(
                opened,
                &project_analysis_cache,
                |progress| {
                    send_operation_progress(
                        &channel,
                        analysis_progress_percent(&progress),
                        &progress.message,
                    );
                },
            )?;
            send_operation_progress(&channel, Some(70), "Review情報を整えています");
            details
        };
        let mod_version = details
            .mods
            .first()
            .and_then(|mod_info| mod_info.version.clone());
        match recent_lock.lock() {
            Ok(_guard) => {
                if let Err(error) = touch_recent_project(&paths, opened.summary(), mod_version) {
                    tracing::warn!(%error.user_message, "recent project record could not be updated");
                }
            }
            Err(_) => tracing::warn!("recent projects lock was poisoned"),
        }
        send_operation_progress(&channel, Some(100), "プロジェクトを開きました");
        Ok(details)
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn get_project_summary(state: State<'_, AppState>) -> Result<ProjectDetails, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let project_analysis_cache = Arc::clone(&state.project_analysis_cache);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| internal_state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| database_error("開いているプロジェクトがありません".to_owned()))?;
        let review = opened
            .repository()
            .review_settings()
            .map_err(|error| database_error(error.to_string()))?;
        if review.setup_completed {
            project_details_from_stored_data(opened, review, None)
        } else {
            project_details_for_all_units(opened, &project_analysis_cache, |_| {})
        }
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn list_mod_units(state: State<'_, AppState>) -> Result<Vec<ModUnitSummary>, AppError> {
    let active_project = Arc::clone(&state.active_project);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| internal_state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| database_error("開いているプロジェクトがありません".to_owned()))?;
        let repository = opened.repository();
        let units = repository
            .list_mod_units()
            .map_err(|error| database_error(error.to_string()))?;
        let documents = repository
            .list_document_entries(None)
            .map_err(|error| database_error(error.to_string()))?;
        units
            .into_iter()
            .map(|unit: ModUnitRecord| {
                let summary = repository
                    .entry_summary(Some(unit.id))
                    .map_err(|error| database_error(error.to_string()))?;
                Ok(ModUnitSummary {
                    id: unit.id,
                    source_kind: unit.source_kind,
                    source_filename: unit.source_filename,
                    mod_id: unit.mod_id,
                    name: unit.name,
                    version: unit.version,
                    loader: unit.loader,
                    minecraft_version: unit.minecraft_version,
                    total: summary.total,
                    translated: summary.translated,
                    needs_review: summary.needs_review,
                    ftb_quest_entries: documents
                        .iter()
                        .filter(|entry| {
                            entry.mod_unit_id == unit.id
                                && entry.entry_kind == yc_project::TranslationEntryKind::Ftbquests
                        })
                        .count(),
                })
            })
            .collect()
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn refresh_modpack_unit(
    mod_unit_id: i64,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let active_project = Arc::clone(&state.active_project);
    let project_analysis_cache = Arc::clone(&state.project_analysis_cache);
    let project_asset_index_cache = Arc::clone(&state.project_asset_index_cache);
    let preview_context = Arc::clone(&state.preview_context);
    let ai_job_tokens = Arc::clone(&state.ai_job_tokens);
    let recent_lock = Arc::clone(&state.recent_lock);
    let paths = state.paths.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut active = active_project
            .lock()
            .map_err(|_| internal_state_error("active Project state"))?;
        let opened = active
            .as_mut()
            .ok_or_else(|| database_error("開いているプロジェクトがありません".to_owned()))?;
        let project_id = opened.summary().id.clone();
        if ai_job_tokens
            .lock()
            .map_err(|_| internal_state_error("AI job state"))?
            .values()
            .any(|job| job.project_id == project_id)
        {
            return Err(AppError::simple(
                "ai_job_in_progress",
                "AI翻訳中は Modpack を再取り込みできません。",
            ));
        }
        refresh_modpack_unit_core(opened, mod_unit_id)?;
        {
            let mut cache = project_analysis_cache
                .lock()
                .map_err(|_| internal_state_error("Project analysis cache"))?;
            if let Some(cache) = cache.as_mut().filter(|cache| cache.project_id == project_id) {
                cache.analyses.remove(&mod_unit_id);
            }
        }
        {
            let mut cache = project_asset_index_cache
                .lock()
                .map_err(|_| internal_state_error("Project asset index cache"))?;
            if let Some(cache) = cache.as_mut().filter(|cache| cache.project_id == project_id) {
                cache.indices.remove(&mod_unit_id);
            }
        }
        {
            let mut cached = preview_context
                .lock()
                .map_err(|_| internal_state_error("preview cache"))?;
            if cached
                .as_ref()
                .is_some_and(|context| context.belongs_to_unit(&project_id, mod_unit_id))
            {
                *cached = None;
            }
        }
        super::preview::clear_mod_icon_cache()?;
        let summary = opened
            .refresh_summary()
            .map_err(|error| database_error(error.to_string()))?;
        match recent_lock.lock() {
            Ok(_guard) => {
                if let Err(error) = refresh_recent_project_progress(&paths, &summary) {
                    tracing::warn!(%error.user_message, "recent project record could not be updated after Modpack refresh");
                }
            }
            Err(error) => tracing::warn!(%error, "recent projects lock was poisoned after Modpack refresh"),
        }
        Ok(())
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

fn refresh_modpack_unit_core(opened: &mut OpenedProject, mod_unit_id: i64) -> Result<(), AppError> {
    let repository = opened.repository();
    let unit = repository
        .list_mod_units()
        .map_err(|error| database_error(error.to_string()))?
        .into_iter()
        .find(|unit| unit.id == mod_unit_id && unit.source_kind == ModUnitSourceKind::Modpack)
        .ok_or_else(|| AppError::simple("modpack_unit_required", "Modpack が見つかりません。"))?;
    let recorded_game_directory = PathBuf::from(&unit.original_path);
    let instance_root = match recorded_game_directory
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("minecraft" | ".minecraft") => recorded_game_directory
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                AppError::simple(
                    "modpack_instance_missing",
                    "Modpack のインスタンスフォルダーを確認できません。",
                )
            })?,
        _ => recorded_game_directory,
    };
    ensure_modpack_translation_directories_accessible(&instance_root)?;
    let scan = yc_modpack::scan_instance(&instance_root);
    refresh_modpack_unit_from_scan(opened, mod_unit_id, &instance_root, scan)
}

fn ensure_modpack_translation_directories_accessible(root: &Path) -> Result<(), AppError> {
    let game_directory = yc_modpack::resolve_game_directory(root);
    let ftb_lang = game_directory.join("config/ftbquests/quests/lang");
    check_optional_modpack_directory(&ftb_lang)?;

    let kubejs_assets = game_directory.join("kubejs/assets");
    let namespaces = match fs::read_dir(&kubejs_assets) {
        Ok(namespaces) => namespaces,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(modpack_directory_access_error(&kubejs_assets, error)),
    };
    for namespace in namespaces {
        let namespace =
            namespace.map_err(|error| modpack_directory_access_error(&kubejs_assets, error))?;
        let is_directory = namespace
            .file_type()
            .map_err(|error| modpack_directory_access_error(&namespace.path(), error))?
            .is_dir();
        if is_directory {
            check_optional_modpack_directory(&namespace.path().join("lang"))?;
        }
    }
    Ok(())
}

fn check_optional_modpack_directory(path: &Path) -> Result<(), AppError> {
    match fs::metadata(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(modpack_directory_access_error(path, error)),
    }
}

fn modpack_directory_access_error(path: &Path, error: impl std::fmt::Display) -> AppError {
    let detail = format!("{}: {error}", path.display());
    AppError::new(
        "modpack_scan_incomplete",
        &format!(
            "Modpack の翻訳ディレクトリを確認できませんでした。アクセス権を確認して再試行してください: {detail}"
        ),
        Some(detail),
    )
}

fn refresh_modpack_unit_from_scan(
    opened: &mut OpenedProject,
    mod_unit_id: i64,
    instance_root: &Path,
    scan: yc_modpack::ModpackScan,
) -> Result<(), AppError> {
    let repository = opened.repository();
    if !scan.issues.is_empty() {
        let detail = scan.issues.join("\n");
        return Err(AppError::new(
            "modpack_scan_incomplete",
            &format!(
                "Modpack の翻訳ファイルを確認できませんでした。次のファイルを確認して再試行してください: {detail}"
            ),
            Some(detail),
        ));
    }
    if scan.files.is_empty() {
        return Err(AppError::simple(
            "modpack_files_missing",
            "Modpack の翻訳ファイルが見つかりません。インスタンスを確認してください。",
        ));
    }
    let files = super::modpack::collect_instance_files(instance_root, &scan)?;
    let game_directory = if scan.game_directory == "." {
        instance_root.to_path_buf()
    } else {
        instance_root.join(&scan.game_directory)
    };
    let canonical_game_directory =
        fs::canonicalize(&game_directory).map_err(modpack_refresh_io_error)?;
    let archive_bytes = super::modpack::write_modpack_source_archive(&files)?;
    let source_path = repository
        .mod_unit_source_path(mod_unit_id)
        .map_err(|error| database_error(error.to_string()))?;
    let refresh_path =
        source_path.with_file_name(format!("{}.refresh-{}.zip", mod_unit_id, Uuid::new_v4()));
    fs::write(&refresh_path, archive_bytes).map_err(modpack_refresh_io_error)?;
    let result = (|| {
        let (analysis, data) = super::modpack::analyze_modpack_archive(&refresh_path)?;
        let saved_entries = repository
            .list_entries(Some(mod_unit_id))
            .map_err(|error| database_error(error.to_string()))?;
        let old_settings = repository
            .review_settings_for_unit(mod_unit_id)
            .map_err(|error| database_error(error.to_string()))?;
        let old_namespace_selection: HashMap<_, _> = repository
            .project_metadata()
            .map_err(|error| database_error(error.to_string()))?
            .namespaces
            .iter()
            .filter(|namespace| namespace.mod_unit_id == mod_unit_id)
            .map(|namespace| (namespace.namespace.clone(), namespace.included))
            .collect();
        let mut settings = analysis.default_review_settings();
        let default_included: HashSet<_> = settings.included_namespaces.iter().cloned().collect();
        settings.source_locale = resolve_source_locale(&analysis, &old_settings.source_locale);
        settings.fallback_priority = old_settings.fallback_priority;
        settings.preserve_existing_japanese = old_settings.preserve_existing_japanese;
        settings.fill_missing_from_other_languages = old_settings.fill_missing_from_other_languages;
        settings.included_namespaces = analysis
            .result
            .namespaces
            .iter()
            .filter(|namespace| {
                old_namespace_selection
                    .get(&namespace.namespace)
                    .copied()
                    .unwrap_or_else(|| default_included.contains(&namespace.namespace))
            })
            .map(|namespace| namespace.namespace.clone())
            .collect();
        let mut merged = merge_for_review(&analysis, &settings);
        exclude_empty_modpack_entries(&mut merged.entries);
        preserve_saved_translations(&mut merged.entries, saved_entries);
        mark_existing_placeholder_errors(&mut merged.entries);
        let project_analysis = project_analysis_data(&analysis, merged.entries, &settings);
        let source_sha256 = yc_archive::sha256_file(&refresh_path)
            .map_err(modpack_refresh_io_error)?
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let backup_path = source_path.with_file_name(format!(
            "{}.refresh-backup-{}.zip",
            mod_unit_id,
            Uuid::new_v4()
        ));
        fs::rename(&source_path, &backup_path).map_err(modpack_refresh_io_error)?;
        if let Err(error) = fs::rename(&refresh_path, &source_path) {
            let _ = fs::rename(&backup_path, &source_path);
            return Err(modpack_refresh_io_error(error));
        }
        if let Err(error) = opened.writer().update_modpack_source(
            mod_unit_id,
            source_sha256,
            canonical_game_directory.to_string_lossy().into_owned(),
        ) {
            let _ = fs::remove_file(&source_path);
            let _ = fs::rename(&backup_path, &source_path);
            return Err(database_error(error.to_string()));
        }
        opened
            .writer()
            .save_analysis(mod_unit_id, project_analysis)
            .map_err(|error| database_error(error.to_string()))?;
        opened
            .writer()
            .save_document_entries(mod_unit_id, modpack_document_entries(&data, &settings))
            .map_err(|error| database_error(error.to_string()))?;
        opened
            .writer()
            .save_review_settings(mod_unit_id, settings, true)
            .map_err(|error| database_error(error.to_string()))?;
        opened
            .writer()
            .save_asset_index(mod_unit_id, ProjectAssetIndex::default())
            .map_err(|error| database_error(error.to_string()))?;
        if let Err(error) = fs::remove_file(backup_path) {
            tracing::warn!(%error, "previous Modpack source archive could not be removed after refresh");
        }
        Ok(())
    })();
    if let Err(error) = fs::remove_file(&refresh_path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(path = %refresh_path.display(), %error, "temporary Modpack refresh archive could not be removed");
    }
    result
}

#[tauri::command]
pub async fn get_review_asset_summary(
    settings: ReviewSettingsInput,
    channel: Channel<OperationProgress>,
    state: State<'_, AppState>,
) -> Result<ReviewSetupSummary, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let project_analysis_cache = Arc::clone(&state.project_analysis_cache);
    let project_asset_index_cache = Arc::clone(&state.project_asset_index_cache);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| internal_state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| database_error("開いているプロジェクトがありません".to_owned()))?;
        send_operation_progress(&channel, Some(0), "確認項目を読み込んでいます");
        let units = opened
            .repository()
            .list_mod_units()
            .map_err(|error| database_error(error.to_string()))?;
        let mut aggregate: Option<ReviewSetupSummary> = None;
        for unit in units {
            let analysis =
                load_project_analysis(opened, unit.id, &project_analysis_cache, |progress| {
                    send_operation_progress(
                        &channel,
                        analysis_progress_percent(&progress),
                        &progress.message,
                    );
                })?;
            if unit.source_kind == ModUnitSourceKind::Modpack {
                let settings = review_settings_from_input(&analysis, &settings, unit.id)?;
                let merged = merge_for_review(&analysis, &settings);
                let existing_documents = opened
                    .repository()
                    .list_document_entries(Some(unit.id))
                    .map_err(|error| database_error(error.to_string()))?
                    .iter()
                    .filter(|entry| {
                        entry
                            .target_text
                            .as_deref()
                            .is_some_and(|text| !text.trim().is_empty())
                    })
                    .count();
                add_review_setup_summary(
                    &mut aggregate,
                    ReviewSetupSummary {
                        existing_japanese_translation_count: merged
                            .existing_japanese_translation_count
                            + existing_documents,
                        missing_source_keys_count: merged
                            .entries
                            .iter()
                            .filter(|entry| {
                                entry
                                    .source_text
                                    .as_deref()
                                    .is_some_and(|source| !source.trim().is_empty())
                                    && entry.target_text.trim().is_empty()
                            })
                            .count(),
                        assets: ProjectAssetSummary {
                            total: 0,
                            block_models: 0,
                            item_models: 0,
                            entity_textures: 0,
                            geometry_models: 0,
                            unresolved_entries: 0,
                        },
                    },
                );
                cache_project_asset_index(
                    &project_asset_index_cache,
                    &opened.summary().id,
                    unit.id,
                    &settings.included_namespaces,
                    ProjectAssetIndex::default(),
                )?;
                continue;
            }
            let (unit_summary, index) = review_asset_summary_core_with_index_and_progress(
                opened,
                unit.id,
                &settings,
                &analysis,
                |percent, message| send_operation_progress(&channel, percent, message),
            )?;
            add_review_setup_summary(&mut aggregate, unit_summary);
            let unit_namespaces = selected_namespaces(&settings, unit.id);
            cache_project_asset_index(
                &project_asset_index_cache,
                &opened.summary().id,
                unit.id,
                &unit_namespaces,
                index,
            )?;
        }
        send_operation_progress(&channel, Some(100), "確認項目を読み込みました");
        aggregate.ok_or_else(|| database_error("プロジェクトにmodがありません".to_owned()))
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

fn selected_namespaces(settings: &ReviewSettingsInput, mod_unit_id: i64) -> Vec<String> {
    settings
        .included_namespaces
        .iter()
        .filter(|selection| selection.mod_unit_id == mod_unit_id)
        .map(|selection| selection.namespace.clone())
        .collect()
}

fn add_review_setup_summary(aggregate: &mut Option<ReviewSetupSummary>, unit: ReviewSetupSummary) {
    if let Some(aggregate) = aggregate {
        aggregate.existing_japanese_translation_count += unit.existing_japanese_translation_count;
        aggregate.missing_source_keys_count += unit.missing_source_keys_count;
        aggregate.assets.total += unit.assets.total;
        aggregate.assets.block_models += unit.assets.block_models;
        aggregate.assets.item_models += unit.assets.item_models;
        aggregate.assets.entity_textures += unit.assets.entity_textures;
        aggregate.assets.geometry_models += unit.assets.geometry_models;
        aggregate.assets.unresolved_entries += unit.assets.unresolved_entries;
    } else {
        *aggregate = Some(unit);
    }
}

#[tauri::command]
pub async fn save_review_settings(
    settings: ReviewSettingsInput,
    channel: Channel<OperationProgress>,
    state: State<'_, AppState>,
) -> Result<ProjectDetails, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let project_analysis_cache = Arc::clone(&state.project_analysis_cache);
    let project_asset_index_cache = Arc::clone(&state.project_asset_index_cache);
    let paths = state.paths.clone();
    let recent_lock = Arc::clone(&state.recent_lock);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| internal_state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| database_error("開いているプロジェクトがありません".to_owned()))?;
        send_operation_progress(&channel, Some(0), "翻訳設定を保存しています");
        let units = opened
            .repository()
            .list_mod_units()
            .map_err(|error| database_error(error.to_string()))?;
        let mut existing_japanese_translation_count = 0;
        let mut missing_source_keys_count = 0;
        let mut first_signed = None;
        let setup_unit_id = units.first().map(|unit| unit.id);
        for unit in &units {
            let analysis = load_project_analysis(
                opened,
                unit.id,
                &project_analysis_cache,
                |progress| {
                    send_operation_progress(
                        &channel,
                        analysis_progress_percent(&progress),
                        &progress.message,
                    );
                },
            )?;
            first_signed.get_or_insert(analysis.result.is_signed);
            if unit.source_kind == ModUnitSourceKind::Modpack {
                let unit_settings = review_settings_from_input(&analysis, &settings, unit.id)?;
                let merged = merge_for_review(&analysis, &unit_settings);
                let mut entries = merged.entries;
                exclude_empty_modpack_entries(&mut entries);
                let saved_entries = opened
                    .repository()
                    .list_entries(Some(unit.id))
                    .map_err(|error| database_error(error.to_string()))?;
                preserve_saved_translations(&mut entries, saved_entries);
                mark_existing_placeholder_errors(&mut entries);
                let project_analysis = project_analysis_data(&analysis, entries, &unit_settings);
                let source_path = opened
                    .repository()
                    .mod_unit_source_path(unit.id)
                    .map_err(|error| database_error(error.to_string()))?;
                let (_, data) = super::modpack::analyze_modpack_archive(&source_path)?;
                opened
                    .writer()
                    .save_analysis(unit.id, project_analysis)
                    .map_err(|error| database_error(error.to_string()))?;
                opened
                    .writer()
                    .save_document_entries(
                        unit.id,
                        modpack_document_entries(&data, &unit_settings),
                    )
                    .map_err(|error| database_error(error.to_string()))?;
                opened
                    .writer()
                    .save_review_settings(unit.id, unit_settings, false)
                    .map_err(|error| database_error(error.to_string()))?;
                opened
                    .writer()
                    .save_asset_index(unit.id, ProjectAssetIndex::default())
                    .map_err(|error| database_error(error.to_string()))?;
                existing_japanese_translation_count += merged.existing_japanese_translation_count
                    + opened
                        .repository()
                        .list_document_entries(Some(unit.id))
                        .map_err(|error| database_error(error.to_string()))?
                        .iter()
                        .filter(|entry| {
                            entry.target_text.as_deref().is_some_and(|text| !text.trim().is_empty())
                        })
                        .count();
                missing_source_keys_count += merged.missing_source_keys_count;
                continue;
            }
            let unit_namespaces = selected_namespaces(&settings, unit.id);
            let cached_asset_index = take_cached_project_asset_index(
                &project_asset_index_cache,
                &opened.summary().id,
                unit.id,
                &unit_namespaces,
            )?;
            let (existing, missing) = save_review_settings_core_with_cached_asset_index(
                opened,
                unit.id,
                &settings,
                &analysis,
                cached_asset_index,
                false,
                |percent, message| send_operation_progress(&channel, percent, message),
            )?;
            existing_japanese_translation_count += existing;
            missing_source_keys_count += missing;
        }
        if let Some(mod_unit_id) = setup_unit_id {
            // 全unitの保存後にだけ完了へ進め、途中失敗したProjectはReviewから再開できるようにする。
            let unit_settings = opened
                .repository()
                .review_settings_for_unit(mod_unit_id)
                .map_err(|error| database_error(error.to_string()))?;
            opened
                .writer()
                .save_review_settings(mod_unit_id, unit_settings, true)
                .map_err(|error| database_error(error.to_string()))?;
        }
        let review = opened
            .repository()
            .review_settings()
            .map_err(|error| database_error(error.to_string()))?;
        // 保存直後はDBの集計を使い、Review画面用のmergeとJAR全走査を重ねない。
        let mut details = project_details_from_stored_data(
            opened,
            review,
            first_signed,
        )?;
        details.existing_japanese_translation_count = existing_japanese_translation_count;
        details.missing_source_keys_count = missing_source_keys_count;
        match recent_lock.lock() {
            Ok(_guard) => {
                if let Err(error) = refresh_recent_project_progress(&paths, opened.summary()) {
                    tracing::warn!(%error.user_message, "recent translation progress could not be updated");
                }
            }
            Err(_) => tracing::warn!("recent projects lock was poisoned"),
        }
        send_operation_progress(&channel, Some(100), "翻訳設定を保存しました");
        Ok(details)
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn list_recent_projects(
    state: State<'_, AppState>,
) -> Result<Vec<RecentProject>, AppError> {
    let paths = state.paths.clone();
    let project_io_lock = Arc::clone(&state.project_io_lock);
    let recent_lock = Arc::clone(&state.recent_lock);
    tauri::async_runtime::spawn_blocking(move || {
        // 作成処理と同じロックで列挙し、解析中のProjectを削除可能なRecent行に出さない。
        let _project_io = project_io_lock
            .lock()
            .map_err(|_| internal_state_error("Project file operations"))?;
        let _recent_guard = recent_lock
            .lock()
            .map_err(|_| internal_state_error("recent projects lock"))?;
        list_recent_projects_core(&paths)
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn rename_project(
    project_id: String,
    name: String,
    state: State<'_, AppState>,
) -> Result<ProjectSummary, AppError> {
    let id = Uuid::parse_str(&project_id).map_err(|error| {
        AppError::new(
            "project_not_found",
            "プロジェクト名を変更できませんでした。",
            Some(error.to_string()),
        )
    })?;
    let name = normalize_project_name(&name)
        .map_err(|error| AppError::simple("invalid_project_name", &error.to_string()))?;
    let project_key = id.to_string();
    let paths = state.paths.clone();
    let active_project = Arc::clone(&state.active_project);
    let project_io_lock = Arc::clone(&state.project_io_lock);
    let deleting_projects = Arc::clone(&state.deleting_projects);
    let recent_lock = Arc::clone(&state.recent_lock);
    tauri::async_runtime::spawn_blocking(move || {
        let deleting = deleting_projects
            .lock()
            .map_err(|_| internal_state_error("Project deletion state"))?;
        if deleting.contains(&project_key) {
            return Err(AppError::simple(
                "project_delete_in_progress",
                "このプロジェクトは削除中です。",
            ));
        }
        let _project_io = project_io_lock
            .lock()
            .map_err(|_| internal_state_error("Project file operations"))?;
        let mut active = active_project
            .lock()
            .map_err(|_| internal_state_error("active Project state"))?;
        let summary = match active.as_mut() {
            Some(opened) if opened.summary().id == project_key => {
                opened.rename(&name).map_err(|error| {
                    AppError::new(
                        "project_rename_failed",
                        "プロジェクト名を変更できませんでした。",
                        Some(error.to_string()),
                    )
                })?
            }
            _ => rename_project_file(&paths.projects_dir, id, &name).map_err(|error| {
                AppError::new(
                    "project_rename_failed",
                    "プロジェクト名を変更できませんでした。",
                    Some(error.to_string()),
                )
            })?,
        };

        let _recent = recent_lock
            .lock()
            .map_err(|_| internal_state_error("recent projects lock"))?;
        let mut recent = load_recent_projects(&paths.recent_file)
            .map_err(|error| database_error(error.to_string()))?;
        if let Some(project) = recent
            .projects
            .iter_mut()
            .find(|project| project.id == project_key)
        {
            project.display_name.clone_from(&summary.display_name);
            project.mod_count = summary.mod_count;
            project.instance_path.clone_from(&summary.instance_path);
        } else {
            recent.projects.push(RecentProject {
                id: summary.id.clone(),
                display_name: summary.display_name.clone(),
                mod_version: None,
                mod_count: summary.mod_count,
                instance_path: summary.instance_path.clone(),
                last_opened: summary.updated_at.clone(),
                translation_progress: TranslationProgress::default(),
            });
        }
        save_recent_projects(&paths.recent_file, &recent).map_err(|error| {
            AppError::new(
                "recent_update_failed",
                "プロジェクト名は変更されましたが、最近の一覧を更新できませんでした。",
                Some(error.to_string()),
            )
        })?;
        Ok(summary)
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn get_mod_unit_removal_info(
    mod_unit_id: i64,
    state: State<'_, AppState>,
) -> Result<ModUnitRemovalInfo, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let project_io_lock = Arc::clone(&state.project_io_lock);
    tauri::async_runtime::spawn_blocking(move || {
        let _project_io = project_io_lock
            .lock()
            .map_err(|_| internal_state_error("Project file operations"))?;
        let active = active_project
            .lock()
            .map_err(|_| internal_state_error("active Project state"))?;
        let opened = active.as_ref().ok_or_else(|| {
            AppError::simple("project_not_open", "プロジェクトを開けませんでした。")
        })?;
        let has_unrestored_replacements = opened
            .repository()
            .has_unrestored_jar_replacements(mod_unit_id)
            .map_err(|error| database_error(error.to_string()))?;
        let has_unrestored_file_deployments = opened
            .repository()
            .has_unrestored_file_deployments(mod_unit_id)
            .map_err(|error| database_error(error.to_string()))?;
        Ok(ModUnitRemovalInfo {
            has_unrestored_replacements,
            has_unrestored_file_deployments,
        })
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn remove_mod_unit(
    mod_unit_id: i64,
    confirm_unrestored_replacements: bool,
    state: State<'_, AppState>,
) -> Result<ProjectSummary, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let project_io_lock = Arc::clone(&state.project_io_lock);
    let deleting_projects = Arc::clone(&state.deleting_projects);
    let ai_job_tokens = Arc::clone(&state.ai_job_tokens);
    let project_analysis_cache = Arc::clone(&state.project_analysis_cache);
    let project_asset_index_cache = Arc::clone(&state.project_asset_index_cache);
    let preview_context = Arc::clone(&state.preview_context);
    let recent_lock = Arc::clone(&state.recent_lock);
    let paths = state.paths.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let deleting = deleting_projects
            .lock()
            .map_err(|_| internal_state_error("Project deletion state"))?;
        let _project_io = project_io_lock
            .lock()
            .map_err(|_| internal_state_error("Project file operations"))?;
        let mut active = active_project
            .lock()
            .map_err(|_| internal_state_error("active Project state"))?;
        let opened = active
            .as_mut()
            .ok_or_else(|| AppError::simple("project_not_open", "プロジェクトを開けませんでした。"))?;
        let project_id = opened.summary().id.clone();
        if deleting.contains(&project_id) {
            return Err(AppError::simple(
                "project_delete_in_progress",
                "このプロジェクトは削除中です。",
            ));
        }
        if ai_job_tokens
            .lock()
            .map_err(|_| internal_state_error("AI job state"))?
            .values()
            .any(|job| job.project_id == project_id)
        {
            return Err(AppError::simple("ai_job_in_progress", "AI翻訳中はmodを外せません。"));
        }

        let repository = opened.repository();
        if !repository.list_mod_units().map_err(|error| database_error(error.to_string()))?
            .iter()
            .any(|unit| unit.id == mod_unit_id)
        {
            return Err(AppError::simple("mod_unit_not_found", "modを見つけられませんでした。"));
        }
        let has_unrestored_replacements = repository
            .has_unrestored_jar_replacements(mod_unit_id)
            .map_err(|error| database_error(error.to_string()))?;
        let has_unrestored_file_deployments = repository
            .has_unrestored_file_deployments(mod_unit_id)
            .map_err(|error| database_error(error.to_string()))?;
        if (has_unrestored_replacements || has_unrestored_file_deployments)
            && !confirm_unrestored_replacements
        {
            return Err(AppError::simple(
                "unit_deployments_pending",
                "配置したファイルのバックアップを復元できなくなります。",
            ));
        }
        if repository.list_mod_units().map_err(|error| database_error(error.to_string()))?.len() <= 1 {
            return Err(AppError::simple("last_mod_unit", "最後のmodはプロジェクトから外せません。"));
        }

        {
            let mut cached = project_analysis_cache
                .lock()
                .map_err(|_| internal_state_error("Project analysis cache"))?;
            if let Some(cache) = cached.as_mut().filter(|cache| cache.project_id == project_id) {
                cache.analyses.remove(&mod_unit_id);
            }
        }
        {
            let mut cached = project_asset_index_cache
                .lock()
                .map_err(|_| internal_state_error("Project asset index cache"))?;
            if let Some(cache) = cached.as_mut().filter(|cache| cache.project_id == project_id) {
                cache.indices.remove(&mod_unit_id);
            }
        }
        {
            let mut cached = preview_context
                .lock()
                .map_err(|_| internal_state_error("preview cache"))?;
            if cached
                .as_ref()
                .is_some_and(|context| context.belongs_to_unit(&project_id, mod_unit_id))
            {
                *cached = None;
            }
        }
        super::preview::clear_mod_icon_cache()?;

        let summary = opened.remove_mod_unit(mod_unit_id).map_err(|error| match error {
            yc_project::ProjectError::LastModUnit => {
                AppError::simple("last_mod_unit", "最後のmodはプロジェクトから外せません。")
            }
            _ => database_error(error.to_string()),
        })?;
        match recent_lock.lock() {
            Ok(_recent) => {
                if let Err(error) = refresh_recent_project_progress(&paths, &summary) {
                    tracing::warn!(%error.user_message, "recent project record could not be updated after mod removal");
                }
            }
            Err(error) => tracing::warn!(%error, "recent projects lock was poisoned after mod removal"),
        }
        Ok(summary)
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn delete_project(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let id = Uuid::parse_str(&project_id).map_err(|error| {
        AppError::new(
            "project_not_found",
            "プロジェクトを削除できませんでした。",
            Some(error.to_string()),
        )
    })?;
    let project_key = id.to_string();
    let _deletion_guard = state.cancel_and_wait_project_ai_jobs(&project_key).await?;

    let paths = state.paths.clone();
    let active_project = Arc::clone(&state.active_project);
    let project_io_lock = Arc::clone(&state.project_io_lock);
    let project_analysis_cache = Arc::clone(&state.project_analysis_cache);
    let project_asset_index_cache = Arc::clone(&state.project_asset_index_cache);
    let preview_context = Arc::clone(&state.preview_context);
    let recent_lock = Arc::clone(&state.recent_lock);
    tauri::async_runtime::spawn_blocking(move || {
        let _project_io = prepare_project_for_deletion(
            &project_io_lock,
            &active_project,
            &project_analysis_cache,
            &project_asset_index_cache,
            &preview_context,
            &project_key,
        )?;
        remove_project(&paths.projects_dir, id).map_err(project_delete_error)?;

        // rename成功で削除は確定済み。Recent更新失敗を削除失敗として見せず、一覧取得で実体から補正する。
        match recent_lock.lock() {
            Ok(_recent_guard) => match load_recent_projects(&paths.recent_file) {
                Ok(mut recent) => {
                    recent.projects.retain(|project| project.id != project_key);
                    if let Err(error) = save_recent_projects(&paths.recent_file, &recent) {
                        tracing::warn!(project_id = %project_key, %error, "deleted Project could not be removed from Recent records");
                    }
                }
                Err(error) => tracing::warn!(project_id = %project_key, %error, "Recent records could not be loaded after Project deletion"),
            },
            Err(error) => tracing::warn!(project_id = %project_key, %error, "Recent lock was poisoned after Project deletion"),
        }
        Ok(())
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

pub(crate) fn prepare_project_for_deletion<'a>(
    project_io_lock: &'a std::sync::Mutex<()>,
    active_project: &std::sync::Mutex<Option<OpenedProject>>,
    project_analysis_cache: &std::sync::Mutex<Option<CachedProjectAnalysis>>,
    project_asset_index_cache: &std::sync::Mutex<Option<CachedProjectAssetIndex>>,
    preview_context: &std::sync::Mutex<Option<crate::commands::preview::PreviewContext>>,
    project_id: &str,
) -> Result<MutexGuard<'a, ()>, AppError> {
    let guard = project_io_lock
        .lock()
        .map_err(|_| internal_state_error("Project file operations"))?;
    {
        let mut active = active_project
            .lock()
            .map_err(|_| internal_state_error("active Project state"))?;
        if active
            .as_ref()
            .is_some_and(|opened| opened.summary().id == project_id)
        {
            // Writerのdropで保存キューを完了し、SQLite接続とProjectLockを閉じてからrenameする。
            drop(active.take());
        }
    }
    {
        let mut cached = project_analysis_cache
            .lock()
            .map_err(|_| internal_state_error("Project analysis cache"))?;
        if cached
            .as_ref()
            .is_some_and(|analysis| analysis.project_id == project_id)
        {
            *cached = None;
        }
    }
    {
        let mut cached = project_asset_index_cache
            .lock()
            .map_err(|_| internal_state_error("Project asset index cache"))?;
        if cached
            .as_ref()
            .is_some_and(|index| index.project_id == project_id)
        {
            *cached = None;
        }
    }
    let mut cached_preview = preview_context
        .lock()
        .map_err(|_| internal_state_error("preview cache"))?;
    if cached_preview
        .as_ref()
        .is_some_and(|context| context.project_id == project_id)
    {
        // PreviewContextは対象unitのJARとMinecraft本体JARのArchive readerを所有する。
        *cached_preview = None;
    }
    Ok(guard)
}

fn project_delete_error(error: yc_project::ProjectError) -> AppError {
    match error {
        yc_project::ProjectError::ProjectDeleteRename(source) => {
            tracing::warn!(%source, "Project could not be moved to trash");
            AppError::simple(
                "project_delete_rename_failed",
                "ほかのアプリがファイルを使用しています。しばらくしてからもう一度お試しください。",
            )
        }
        yc_project::ProjectError::ProjectAlreadyOpen => AppError::simple(
            "project_in_use",
            "別のアプリでプロジェクトが開かれています。閉じてからもう一度お試しください。",
        ),
        other => {
            tracing::warn!(%other, "Project deletion failed before the move to trash");
            AppError::simple(
                "project_delete_failed",
                "プロジェクトを削除できませんでした。もう一度お試しください。",
            )
        }
    }
}

pub(crate) fn create_project_from_sources_core(
    paths: &AppPaths,
    sources: &[PathBuf],
    instance_path: Option<&Path>,
    include_modpack: bool,
    cancellation: &CancellationToken,
    mut on_progress: impl FnMut(AnalysisProgress, usize, usize, String),
) -> Result<CreatedProject, AppError> {
    let modpack_data = if include_modpack {
        let instance_root = instance_path.ok_or_else(|| {
            AppError::simple(
                "modpack_instance_required",
                "インスタンスを選び直してください。",
            )
        })?;
        let scan = yc_modpack::scan_instance(instance_root);
        if scan.files.is_empty() {
            return Err(AppError::simple(
                "modpack_files_missing",
                "Modpack の翻訳ファイルが見つかりません。再スキャンしてください。",
            ));
        }
        let files = super::modpack::collect_instance_files(instance_root, &scan)?;
        let game_directory = if scan.game_directory == "." {
            instance_root.to_path_buf()
        } else {
            instance_root.join(&scan.game_directory)
        };
        Some((game_directory, files))
    } else {
        None
    };
    let modpack_source = modpack_data
        .as_ref()
        .map(|(game_directory, files)| ModpackSource {
            game_directory,
            files,
        });
    let summary =
        create_project_with_modpack(&paths.projects_dir, sources, instance_path, modpack_source)
            .map_err(|error| database_error(error.to_string()))?;
    let project_id = summary.id.clone();
    let result = (|| {
        let opened = open_project_file(
            &paths.projects_dir,
            Uuid::parse_str(&project_id).map_err(|error| database_error(error.to_string()))?,
        )
        .map_err(|error| database_error(error.to_string()))?;
        let units = opened
            .repository()
            .list_mod_units()
            .map_err(|error| database_error(error.to_string()))?;
        let mod_count = units.len();
        let mut failures = Vec::new();
        let mut analyses = HashMap::with_capacity(mod_count);
        let mut mod_version = None;

        for (index, unit) in units.into_iter().enumerate() {
            if cancellation.is_cancelled() {
                return Err(cancelled_error());
            }
            let source_path = opened
                .repository()
                .mod_unit_source_path(unit.id)
                .map_err(|error| database_error(error.to_string()))?;
            let filename = unit.source_filename.clone();
            if unit.source_kind == ModUnitSourceKind::Modpack {
                let (analysis, data) = super::modpack::analyze_modpack_archive(&source_path)?;
                let mut settings = analysis.default_review_settings();
                settings.included_namespaces = data.scan.kubejs_namespaces.clone();
                let mut merged = merge_for_review(&analysis, &settings);
                exclude_empty_modpack_entries(&mut merged.entries);
                mark_existing_placeholder_errors(&mut merged.entries);
                let project_analysis = project_analysis_data(&analysis, merged.entries, &settings);
                opened
                    .writer()
                    .save_analysis(unit.id, project_analysis)
                    .map_err(|error| database_error(error.to_string()))?;
                opened
                    .writer()
                    .save_document_entries(unit.id, modpack_document_entries(&data, &settings))
                    .map_err(|error| database_error(error.to_string()))?;
                opened
                    .writer()
                    .save_review_settings(unit.id, settings, false)
                    .map_err(|error| database_error(error.to_string()))?;
                opened
                    .writer()
                    .save_asset_index(unit.id, ProjectAssetIndex::default())
                    .map_err(|error| database_error(error.to_string()))?;
                analyses.insert(unit.id, Arc::new(analysis));
                continue;
            }
            let analysis = yc_analysis::analyze_jar(&source_path, cancellation, |progress| {
                on_progress(progress, index + 1, mod_count, filename.clone());
            });
            let analysis = match analysis {
                Ok(analysis) if analysis.result.source_sha256 == unit.source_sha256 => {
                    Arc::new(analysis)
                }
                Ok(_) => {
                    failures.push(ModImportFailure {
                        filename: filename.clone(),
                        reason: "読み込み中にファイルの内容が変わりました。".to_owned(),
                    });
                    remove_failed_mod_unit(&opened, unit.id, &source_path)?;
                    continue;
                }
                Err(_error) if cancellation.is_cancelled() => return Err(cancelled_error()),
                Err(error) => {
                    failures.push(ModImportFailure {
                        filename: filename.clone(),
                        reason: analysis_error(error).user_message,
                    });
                    remove_failed_mod_unit(&opened, unit.id, &source_path)?;
                    continue;
                }
            };

            let settings = analysis.default_review_settings();
            if mod_version.is_none() {
                mod_version = analysis
                    .result
                    .mods
                    .first()
                    .and_then(|mod_info| mod_info.version.clone());
            }
            let mut merged = merge_for_review(&analysis, &settings);
            mark_existing_placeholder_errors(&mut merged.entries);
            let project_analysis = project_analysis_data(&analysis, merged.entries, &settings);
            let asset_index =
                asset_index_or_warn(&source_path, &project_analysis.entries, &project_id);
            opened
                .writer()
                .save_analysis(unit.id, project_analysis)
                .map_err(|error| database_error(error.to_string()))?;
            opened
                .writer()
                .save_document_entries(unit.id, document_entries_for_analysis(&analysis, &settings))
                .map_err(|error| database_error(error.to_string()))?;
            opened
                .writer()
                .save_review_settings(unit.id, settings, false)
                .map_err(|error| database_error(error.to_string()))?;
            save_asset_index_or_warn(&opened, unit.id, asset_index, &project_id);
            analyses.insert(unit.id, analysis);
        }

        if cancellation.is_cancelled() {
            return Err(cancelled_error());
        }
        if analyses.is_empty() {
            let detail = failures
                .iter()
                .map(|failure| format!("{}: {}", failure.filename, failure.reason))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(AppError::new(
                "all_mods_failed",
                "JARを読み込めませんでした。ファイルを確認してもう一度お試しください。",
                Some(detail),
            ));
        }
        drop(opened);
        let opened = open_project_file(
            &paths.projects_dir,
            Uuid::parse_str(&project_id).map_err(|error| database_error(error.to_string()))?,
        )
        .map_err(|error| database_error(error.to_string()))?;
        Ok(CreatedProject {
            opened,
            mod_version,
            analyses,
            failed_mods: failures,
        })
    })();

    if result.is_err() {
        // 全unitが失敗したProjectやキャンセルで不完全なProjectをRecentへ残さない。
        cleanup_project(paths, &project_id);
    }
    result
}

#[cfg(test)]
pub(crate) struct SingleModCreatedProject {
    pub opened: OpenedProject,
    pub mod_version: Option<String>,
    pub analysis: Arc<AnalysisData>,
}

// 既存の単体JARフローを検証するテストでは、複数unit対応後も対象unitの解析結果を直接参照する。
#[cfg(test)]
pub(crate) fn create_project_core(
    paths: &AppPaths,
    source: &Path,
    cancellation: &CancellationToken,
    mut on_progress: impl FnMut(AnalysisProgress),
) -> Result<SingleModCreatedProject, AppError> {
    let created = create_project_from_sources_core(
        paths,
        &[source.to_path_buf()],
        None,
        false,
        cancellation,
        |progress, _, _, _| on_progress(progress),
    )?;
    let unit_id = created
        .opened
        .repository()
        .list_mod_units()
        .map_err(|error| database_error(error.to_string()))?
        .first()
        .map(|unit| unit.id)
        .ok_or_else(|| database_error("created Project has no valid mod unit".to_owned()))?;
    let analysis = created
        .analyses
        .get(&unit_id)
        .cloned()
        .ok_or_else(|| database_error("created Project analysis is missing".to_owned()))?;
    Ok(SingleModCreatedProject {
        opened: created.opened,
        mod_version: created.mod_version,
        analysis,
    })
}

fn remove_failed_mod_unit(
    opened: &OpenedProject,
    mod_unit_id: i64,
    source_path: &Path,
) -> Result<(), AppError> {
    opened
        .writer()
        .remove_mod_unit(mod_unit_id)
        .map_err(|error| database_error(error.to_string()))?;
    if let Err(error) = std::fs::remove_file(source_path) {
        tracing::warn!(mod_unit_id, %error, "failed JAR copy could not be removed");
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn save_review_settings_core(
    opened: &OpenedProject,
    input: &ReviewSettingsInput,
) -> Result<(), AppError> {
    let mod_unit_id = first_mod_unit_id(opened)?;
    let source_path = opened
        .repository()
        .mod_unit_source_path(mod_unit_id)
        .map_err(|error| database_error(error.to_string()))?;
    let analysis = yc_analysis::analyze_jar(&source_path, &CancellationToken::default(), |_| {})
        .map_err(analysis_error)?;
    save_review_settings_core_with_cached_asset_index(
        opened,
        mod_unit_id,
        input,
        &analysis,
        None,
        true,
        |_, _| {},
    )
    .map(|_| ())
}

fn save_review_settings_core_with_cached_asset_index(
    opened: &OpenedProject,
    mod_unit_id: i64,
    input: &ReviewSettingsInput,
    analysis: &AnalysisData,
    cached_asset_index: Option<ProjectAssetIndex>,
    setup_completed: bool,
    mut on_progress: impl FnMut(Option<u8>, &str),
) -> Result<(usize, usize), AppError> {
    on_progress(Some(5), "翻訳内容をまとめています");
    let validation_started = Instant::now();
    let settings = review_settings_from_input(analysis, input, mod_unit_id)?;
    tracing::info!(
        stage = "review_settings_validation",
        elapsed_ms = validation_started.elapsed().as_millis(),
        "Review save phase completed"
    );
    let merge_started = Instant::now();
    let mut merged = merge_for_review(analysis, &settings);
    tracing::info!(
        stage = "merge_languages",
        elapsed_ms = merge_started.elapsed().as_millis(),
        entry_count = merged.entries.len(),
        "Review save phase completed"
    );
    let saved_entries = opened
        .repository()
        .list_entries(Some(mod_unit_id))
        .map_err(|error| database_error(error.to_string()))?;
    preserve_saved_translations(&mut merged.entries, saved_entries);
    let placeholder_started = Instant::now();
    mark_existing_placeholder_errors(&mut merged.entries);
    tracing::info!(
        stage = "placeholder_validation",
        elapsed_ms = placeholder_started.elapsed().as_millis(),
        "Review save phase completed"
    );
    let merge_counts = (
        merged.existing_japanese_translation_count,
        merged.missing_source_keys_count,
    );
    let project_analysis = project_analysis_data(analysis, merged.entries, &settings);

    on_progress(Some(30), "アセットとの対応を調べています");
    let asset_index_started = Instant::now();
    let source_path = opened
        .repository()
        .mod_unit_source_path(mod_unit_id)
        .map_err(|error| database_error(error.to_string()))?;
    let asset_index = match cached_asset_index {
        Some(index) if review_asset_index_matches(&index, &project_analysis.entries) => {
            tracing::info!(
                stage = "review_asset_index_cache_hit",
                "Review asset index reused from summary"
            );
            Some(index)
        }
        _ => review_asset_index_or_warn(
            &source_path,
            analysis,
            &settings,
            &project_analysis.entries,
            &opened.summary().id,
        ),
    };
    tracing::info!(
        stage = "asset_index_and_linking",
        elapsed_ms = asset_index_started.elapsed().as_millis(),
        "Review save phase completed"
    );

    on_progress(Some(65), "翻訳データを保存しています");
    let analysis_save_started = Instant::now();
    opened
        .writer()
        .save_analysis(mod_unit_id, project_analysis)
        .map_err(|error| database_error(error.to_string()))?;
    opened
        .writer()
        .save_document_entries(
            mod_unit_id,
            document_entries_for_analysis(analysis, &settings),
        )
        .map_err(|error| database_error(error.to_string()))?;
    tracing::info!(
        stage = "save_analysis_database",
        elapsed_ms = analysis_save_started.elapsed().as_millis(),
        "Review save phase completed"
    );

    let settings_save_started = Instant::now();
    opened
        .writer()
        .save_review_settings(mod_unit_id, settings, setup_completed)
        .map_err(|error| database_error(error.to_string()))?;
    tracing::info!(
        stage = "save_review_settings_database",
        elapsed_ms = settings_save_started.elapsed().as_millis(),
        "Review save phase completed"
    );

    on_progress(None, "アセット情報を保存しています");
    let asset_save_started = Instant::now();
    save_asset_index_or_warn(opened, mod_unit_id, asset_index, &opened.summary().id);
    tracing::info!(
        stage = "save_asset_index_database",
        elapsed_ms = asset_save_started.elapsed().as_millis(),
        "Review save phase completed"
    );
    on_progress(Some(90), "Reviewを整えています");
    Ok(merge_counts)
}

#[cfg(test)]
pub(crate) fn review_asset_summary_core(
    opened: &OpenedProject,
    input: &ReviewSettingsInput,
) -> Result<ReviewSetupSummary, AppError> {
    let mod_unit_id = first_mod_unit_id(opened)?;
    let source_path = opened
        .repository()
        .mod_unit_source_path(mod_unit_id)
        .map_err(|error| database_error(error.to_string()))?;
    let analysis = yc_analysis::analyze_jar(&source_path, &CancellationToken::default(), |_| {})
        .map_err(analysis_error)?;
    review_asset_summary_core_with_analysis(opened, mod_unit_id, input, &analysis)
}

#[cfg(test)]
fn review_asset_summary_core_with_analysis(
    opened: &OpenedProject,
    mod_unit_id: i64,
    input: &ReviewSettingsInput,
    analysis: &AnalysisData,
) -> Result<ReviewSetupSummary, AppError> {
    review_asset_summary_core_with_progress(opened, mod_unit_id, input, analysis)
}

#[cfg(test)]
fn review_asset_summary_core_with_progress(
    opened: &OpenedProject,
    mod_unit_id: i64,
    input: &ReviewSettingsInput,
    analysis: &AnalysisData,
) -> Result<ReviewSetupSummary, AppError> {
    review_asset_summary_core_with_index_and_progress(
        opened,
        mod_unit_id,
        input,
        analysis,
        |_, _| {},
    )
    .map(|(summary, _)| summary)
}

fn review_asset_summary_core_with_index_and_progress(
    opened: &OpenedProject,
    mod_unit_id: i64,
    input: &ReviewSettingsInput,
    analysis: &AnalysisData,
    mut on_progress: impl FnMut(Option<u8>, &str),
) -> Result<(ReviewSetupSummary, ProjectAssetIndex), AppError> {
    on_progress(Some(5), "翻訳内容をまとめています");
    let merge_started = Instant::now();
    let settings = review_settings_from_input(analysis, input, mod_unit_id)?;
    let merged = merge_for_review(analysis, &settings);
    tracing::info!(
        stage = "review_summary_merge",
        elapsed_ms = merge_started.elapsed().as_millis(),
        entry_count = merged.entries.len(),
        "Review summary phase completed"
    );
    on_progress(Some(30), "アセットとの対応を調べています");
    let asset_index_started = Instant::now();
    let source_path = opened
        .repository()
        .mod_unit_source_path(mod_unit_id)
        .map_err(|error| database_error(error.to_string()))?;
    let index = build_review_asset_index(&source_path, analysis, &settings, &merged.entries)
        .map_err(|detail| {
            AppError::new(
                "asset_index_unavailable",
                "アセット情報を読み込めませんでした。同梱Modの選択を変えて再試行してください。",
                Some(detail),
            )
        })?;
    tracing::info!(
        stage = "review_summary_asset_index_and_linking",
        elapsed_ms = asset_index_started.elapsed().as_millis(),
        asset_count = index.assets.len(),
        match_count = index.matches.len(),
        "Review summary phase completed"
    );
    on_progress(Some(85), "確認項目を整えています");
    let asset_type_counts = index
        .assets
        .iter()
        .fold(HashMap::new(), |mut counts, asset| {
            *counts.entry(asset.asset_type.clone()).or_default() += 1;
            counts
        });
    let mut summary = project_asset_summary(&asset_type_counts, &[]);
    summary.unresolved_entries = index
        .matches
        .iter()
        .filter(|entry| entry.missing || entry.ambiguous)
        .count();
    Ok((
        ReviewSetupSummary {
            existing_japanese_translation_count: merged.existing_japanese_translation_count,
            missing_source_keys_count: merged.missing_source_keys_count,
            assets: summary,
        },
        index,
    ))
}

fn review_settings_from_input(
    analysis: &AnalysisData,
    input: &ReviewSettingsInput,
    mod_unit_id: i64,
) -> Result<ReviewSettings, AppError> {
    let available_namespaces: Vec<_> = analysis
        .result
        .namespaces
        .iter()
        .map(|namespace| namespace.namespace.as_str())
        .collect();
    let included_namespaces = selected_namespaces(input, mod_unit_id);
    if included_namespaces
        .iter()
        .any(|selected| !available_namespaces.contains(&selected.as_str()))
    {
        return Err(AppError::simple(
            "invalid_review_settings",
            "同梱Modの選択を読み込めませんでした。名前空間を選び直してください。",
        ));
    }
    let source_locale = resolve_source_locale(analysis, &input.source_locale);

    Ok(ReviewSettings {
        source_locale,
        fallback_priority: input.fallback_priority.clone(),
        preserve_existing_japanese: input.preserve_existing_japanese,
        fill_missing_from_other_languages: input.fill_missing_from_other_languages,
        included_namespaces,
    })
}

fn resolve_source_locale(analysis: &AnalysisData, requested_locale: &str) -> String {
    let normalized_requested = normalize_locale(requested_locale);
    if let Some(language) = analysis
        .result
        .language_files
        .iter()
        .find(|language| normalize_locale(&language.locale) == normalized_requested)
    {
        return language.locale.clone();
    }
    if let Some(language) = analysis
        .result
        .language_files
        .iter()
        .find(|language| normalize_locale(&language.locale) == "en_us")
    {
        return language.locale.clone();
    }
    let mut counts = BTreeMap::<String, (usize, String)>::new();
    for language in &analysis.result.language_files {
        let normalized = normalize_locale(&language.locale);
        let entry = counts
            .entry(normalized)
            .or_insert((0, language.locale.clone()));
        entry.0 += language.entry_count;
    }
    counts
        .into_iter()
        .max_by(|left, right| left.1.0.cmp(&right.1.0).then_with(|| right.0.cmp(&left.0)))
        .map(|(_, (_, locale))| locale)
        .or_else(|| analysis.result.recommended_source_locale.clone())
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn project_details_core(opened: &OpenedProject) -> Result<ProjectDetails, AppError> {
    let mod_unit_id = first_mod_unit_id(opened)?;
    let source_jar = opened
        .repository()
        .mod_unit_source_path(mod_unit_id)
        .map_err(|error| database_error(error.to_string()))?;
    let analysis = yc_analysis::analyze_jar(&source_jar, &CancellationToken::default(), |_| {})
        .map_err(analysis_error)?;
    project_details_core_with_analysis(opened, mod_unit_id, &analysis)
}

fn project_details_core_with_analysis(
    opened: &OpenedProject,
    mod_unit_id: i64,
    analysis: &AnalysisData,
) -> Result<ProjectDetails, AppError> {
    let review = project_review_settings_for_unit(opened, mod_unit_id)?;
    let settings = review_settings_from_record(&review, analysis, mod_unit_id);
    let merged = merge_for_review(analysis, &settings);
    let entries = opened
        .repository()
        .list_entries(Some(mod_unit_id))
        .map_err(|error| database_error(error.to_string()))?;
    let existing_document_translation_count = opened
        .repository()
        .list_document_entries(Some(mod_unit_id))
        .map_err(|error| database_error(error.to_string()))?
        .iter()
        .filter(|entry| {
            entry
                .target_text
                .as_deref()
                .is_some_and(|target| !target.trim().is_empty())
        })
        .count();
    let counts = project_counts_from_summary(
        opened
            .repository()
            .entry_summary(Some(mod_unit_id))
            .map_err(|error| database_error(error.to_string()))?,
    );
    let asset_type_counts = opened
        .repository()
        .asset_type_counts(Some(mod_unit_id))
        .map_err(|error| database_error(error.to_string()))?;
    let asset_summary = project_asset_summary(&asset_type_counts, &entries);
    let mut entry_counts = analysis.result.language_files.iter().fold(
        HashMap::<String, usize>::new(),
        |mut counts, language| {
            *counts.entry(language.namespace.clone()).or_default() += language.entry_count;
            counts
        },
    );
    for entry in &analysis.patchouli_entries {
        *entry_counts.entry(entry.namespace.clone()).or_default() += 1;
    }
    let namespaces = analysis
        .result
        .namespaces
        .iter()
        .filter(|namespace| {
            entry_counts
                .get(&namespace.namespace)
                .copied()
                .unwrap_or_default()
                > 0
        })
        .map(|namespace| NamespaceSummary {
            mod_unit_id,
            namespace: namespace.namespace.clone(),
            container_path: namespace.container_path.clone(),
            included: settings.included_namespaces.contains(&namespace.namespace),
            entry_count: entry_counts
                .get(&namespace.namespace)
                .copied()
                .unwrap_or_default(),
        })
        .collect();
    let languages = analysis
        .result
        .language_files
        .iter()
        .map(|language| LanguageSummary {
            mod_unit_id,
            namespace: language.namespace.clone(),
            locale: language.locale.clone(),
            format: format_label(&language.format).to_owned(),
            entry_count: language.entry_count,
            container_path: language.container_path.clone(),
            archive_path: language.archive_path.clone(),
        })
        .collect();
    let available_included_namespaces: HashSet<_> = entry_counts
        .iter()
        .filter(|(_, entry_count)| **entry_count > 0)
        .map(|(namespace, _)| (mod_unit_id, namespace.as_str()))
        .collect();
    let included_namespaces = review
        .included_namespaces
        .iter()
        .filter(|selection| {
            available_included_namespaces
                .contains(&(selection.mod_unit_id, selection.namespace.as_str()))
        })
        .cloned()
        .collect();
    let source_locale = (!settings.source_locale.trim().is_empty())
        .then(|| settings.source_locale.clone())
        .or_else(|| analysis.result.recommended_source_locale.clone())
        .or_else(|| {
            analysis
                .result
                .language_files
                .first()
                .map(|file| file.locale.clone())
        })
        .unwrap_or_default();
    let discovered = discovered_languages(analysis);
    let first_namespace = settings.included_namespaces.first().or_else(|| {
        analysis
            .result
            .namespaces
            .first()
            .map(|namespace| &namespace.namespace)
    });
    let metadata_ranges: Vec<_> = analysis
        .result
        .mods
        .iter()
        .filter_map(|mod_info| mod_info.minecraft_version.as_deref())
        .collect();
    let minecraft_version_detection =
        yc_export::detect_minecraft_version(&metadata_ranges, None, &[]).or_else(|| {
            opened
                .repository()
                .mod_unit_source_path(mod_unit_id)
                .ok()
                .and_then(|source_jar| {
                    detect_minecraft_version_for_project(&source_jar, &metadata_ranges)
                })
        });
    let (target_language_file_name, target_language_format) = first_namespace
        .and_then(|namespace| {
            yc_language::language_export_filename(&discovered, namespace, &source_locale)
        })
        .map(|file_name| {
            let format = if file_name.to_ascii_lowercase().ends_with(".lang") {
                "legacyLang"
            } else {
                "json"
            };
            (file_name, format.to_owned())
        })
        .unwrap_or_else(|| ("ja_jp.json".to_owned(), "json".to_owned()));

    Ok(ProjectDetails {
        project: opened.summary().clone(),
        mods: analysis
            .result
            .mods
            .iter()
            .map(|mod_info| ModSummary {
                mod_unit_id,
                mod_id: mod_info.mod_id.clone(),
                name: mod_info.name.clone(),
                version: mod_info.version.clone(),
                loader: format!("{:?}", mod_info.loader),
                minecraft_version: mod_info
                    .minecraft_version_estimate
                    .clone()
                    .or_else(|| mod_info.minecraft_version.clone()),
            })
            .collect(),
        namespaces,
        languages,
        nested_archives: analysis
            .result
            .nested_archives
            .iter()
            .map(|archive| NestedArchiveSummary {
                mod_unit_id,
                archive_path: archive.archive_path.clone(),
                mod_id: archive.mod_id.clone(),
                name: archive.name.clone(),
                version: archive.version.clone(),
                has_languages: archive.has_languages,
                skipped_reason: archive.skipped_reason.clone(),
            })
            .collect(),
        recommended_source_locale: analysis.result.recommended_source_locale.clone(),
        source_locale,
        fallback_priority: settings.fallback_priority,
        preserve_existing_japanese: settings.preserve_existing_japanese,
        fill_missing_from_other_languages: settings.fill_missing_from_other_languages,
        included_namespaces,
        setup_completed: review.setup_completed,
        existing_japanese_translation_count: merged.existing_japanese_translation_count
            + existing_document_translation_count,
        missing_source_keys_count: merged.missing_source_keys_count,
        counts,
        target_language_file_name,
        target_language_format,
        is_signed: analysis.result.is_signed,
        asset_count: Some(asset_summary.total),
        asset_summary,
        minecraft_version_detection,
        supported_minecraft_versions: yc_export::supported_minecraft_versions(),
    })
}

pub(crate) fn project_review_settings_for_unit(
    opened: &OpenedProject,
    mod_unit_id: i64,
) -> Result<ProjectReviewSettings, AppError> {
    let repository = opened.repository();
    let mut project_settings = repository
        .review_settings()
        .map_err(|error| database_error(error.to_string()))?;
    let unit_settings = repository
        .review_settings_for_unit(mod_unit_id)
        .map_err(|error| database_error(error.to_string()))?;
    // sourceLocaleはunitごとの存在言語へ解決済みなので、読み直し時も同じunitの値を使う。
    project_settings.source_locale =
        (!unit_settings.source_locale.is_empty()).then_some(unit_settings.source_locale);
    project_settings.fallback_priority = unit_settings.fallback_priority;
    project_settings.preserve_existing_japanese = unit_settings.preserve_existing_japanese;
    project_settings.fill_missing_from_other_languages =
        unit_settings.fill_missing_from_other_languages;
    Ok(project_settings)
}

fn project_details_for_all_units(
    opened: &OpenedProject,
    analysis_cache: &Arc<std::sync::Mutex<Option<CachedProjectAnalysis>>>,
    mut on_progress: impl FnMut(AnalysisProgress),
) -> Result<ProjectDetails, AppError> {
    let units = opened
        .repository()
        .list_mod_units()
        .map_err(|error| database_error(error.to_string()))?;
    let mut aggregate: Option<ProjectDetails> = None;
    for unit in units {
        let analysis = load_project_analysis(opened, unit.id, analysis_cache, |progress| {
            on_progress(progress);
        })?;
        let unit_details = project_details_core_with_analysis(opened, unit.id, &analysis)?;
        if let Some(aggregate) = &mut aggregate {
            aggregate.mods.extend(unit_details.mods);
            aggregate.namespaces.extend(unit_details.namespaces);
            aggregate.languages.extend(unit_details.languages);
            aggregate
                .included_namespaces
                .extend(unit_details.included_namespaces);
            aggregate
                .nested_archives
                .extend(unit_details.nested_archives);
            aggregate.counts.total += unit_details.counts.total;
            aggregate.counts.translated += unit_details.counts.translated;
            aggregate.counts.missing += unit_details.counts.missing;
            aggregate.counts.needs_review += unit_details.counts.needs_review;
            aggregate.existing_japanese_translation_count +=
                unit_details.existing_japanese_translation_count;
            aggregate.missing_source_keys_count += unit_details.missing_source_keys_count;
            aggregate.asset_summary.total += unit_details.asset_summary.total;
            aggregate.asset_summary.block_models += unit_details.asset_summary.block_models;
            aggregate.asset_summary.item_models += unit_details.asset_summary.item_models;
            aggregate.asset_summary.entity_textures += unit_details.asset_summary.entity_textures;
            aggregate.asset_summary.geometry_models += unit_details.asset_summary.geometry_models;
            aggregate.asset_summary.unresolved_entries +=
                unit_details.asset_summary.unresolved_entries;
            aggregate.asset_count = Some(aggregate.asset_summary.total);
            aggregate.is_signed |= unit_details.is_signed;
            if aggregate.minecraft_version_detection.is_none() {
                aggregate.minecraft_version_detection = unit_details.minecraft_version_detection;
            }
        } else {
            aggregate = Some(unit_details);
        }
    }
    aggregate.ok_or_else(|| database_error("プロジェクトにmodがありません".to_owned()))
}

fn project_details_from_stored_data(
    opened: &OpenedProject,
    review: ProjectReviewSettings,
    is_signed: Option<bool>,
) -> Result<ProjectDetails, AppError> {
    let repository = opened.repository();
    let metadata = repository
        .project_metadata()
        .map_err(|error| database_error(error.to_string()))?;
    let entry_summary = repository
        .entry_summary(None)
        .map_err(|error| database_error(error.to_string()))?;
    let asset_type_counts = repository
        .asset_type_counts(None)
        .map_err(|error| database_error(error.to_string()))?;
    let source_locale = review
        .source_locale
        .clone()
        .filter(|locale| !locale.trim().is_empty())
        .or_else(|| {
            metadata
                .languages
                .first()
                .map(|language| language.locale.clone())
        })
        .unwrap_or_default();
    // 名前空間レコードはアセットだけを持つ場合があるため、言語または文書Entryがあるものを選択肢にする。
    let mut namespace_entry_counts = HashMap::<(i64, String), usize>::new();
    for language in &metadata.languages {
        if language.entry_count > 0 {
            *namespace_entry_counts
                .entry((language.mod_unit_id, language.namespace.clone()))
                .or_default() += language.entry_count;
        }
    }
    for entry in repository
        .list_document_entries(None)
        .map_err(|error| database_error(error.to_string()))?
    {
        *namespace_entry_counts
            .entry((entry.mod_unit_id, entry.namespace))
            .or_default() += 1;
    }
    let included_namespaces: Vec<_> = review
        .included_namespaces
        .iter()
        .filter(|selection| {
            namespace_entry_counts
                .get(&(selection.mod_unit_id, selection.namespace.clone()))
                .is_some_and(|entry_count| *entry_count > 0)
        })
        .cloned()
        .collect();
    let included_namespace_set: BTreeSet<_> = included_namespaces
        .iter()
        .map(|selection| (selection.mod_unit_id, selection.namespace.as_str()))
        .collect();
    let namespaces: Vec<_> = metadata
        .namespaces
        .iter()
        .filter(|namespace| {
            namespace_entry_counts
                .get(&(namespace.mod_unit_id, namespace.namespace.clone()))
                .is_some_and(|entry_count| *entry_count > 0)
        })
        .map(|namespace| NamespaceSummary {
            mod_unit_id: namespace.mod_unit_id,
            namespace: namespace.namespace.clone(),
            container_path: namespace.container_path.clone(),
            included: included_namespace_set
                .contains(&(namespace.mod_unit_id, namespace.namespace.as_str())),
            entry_count: namespace_entry_counts
                .get(&(namespace.mod_unit_id, namespace.namespace.clone()))
                .copied()
                .unwrap_or_default(),
        })
        .collect();
    let languages = metadata
        .languages
        .iter()
        .map(|language| LanguageSummary {
            mod_unit_id: language.mod_unit_id,
            namespace: language.namespace.clone(),
            locale: language.locale.clone(),
            format: format_label(&language.format).to_owned(),
            entry_count: language.entry_count,
            container_path: language.container_path.clone(),
            archive_path: language.archive_path.clone(),
        })
        .collect();
    let discovered = discovered_languages_from_records(&metadata.languages);
    let first_namespace = included_namespaces
        .first()
        .map(|selection| &selection.namespace)
        .or_else(|| namespaces.first().map(|namespace| &namespace.namespace));
    let (target_language_file_name, target_language_format) = first_namespace
        .and_then(|namespace| {
            yc_language::language_export_filename(&discovered, namespace, &source_locale)
        })
        .map(|file_name| {
            let format = if file_name.to_ascii_lowercase().ends_with(".lang") {
                "legacyLang"
            } else {
                "json"
            };
            (file_name, format.to_owned())
        })
        .unwrap_or_else(|| ("ja_jp.json".to_owned(), "json".to_owned()));
    let metadata_ranges: Vec<_> = metadata
        .mods
        .iter()
        .filter_map(|mod_info| mod_info.minecraft_version.as_deref())
        .collect();
    let minecraft_version_detection =
        yc_export::detect_minecraft_version(&metadata_ranges, None, &[]).or_else(|| {
            first_mod_unit_id(opened)
                .ok()
                .and_then(|unit_id| repository.mod_unit_source_path(unit_id).ok())
                .and_then(|source_jar| {
                    detect_minecraft_version_for_project(&source_jar, &metadata_ranges)
                })
        });
    let is_signed = match is_signed {
        Some(is_signed) => is_signed,
        None => {
            let jar_unit = repository
                .list_mod_units()
                .map_err(|error| database_error(error.to_string()))?
                .into_iter()
                .find(|unit| unit.source_kind == ModUnitSourceKind::Jar);
            if let Some(unit) = jar_unit {
                let source_path = repository
                    .mod_unit_source_path(unit.id)
                    .map_err(|error| database_error(error.to_string()))?;
                yc_archive::Archive::open(source_path)
                    .map_err(|error| analysis_error(yc_analysis::AnalysisError::Archive(error)))?
                    .is_signed()
            } else {
                false
            }
        }
    };
    let mut asset_summary = project_asset_summary(&asset_type_counts, &[]);
    asset_summary.unresolved_entries = entry_summary.unresolved_assets;

    Ok(ProjectDetails {
        project: opened.summary().clone(),
        mods: metadata
            .mods
            .iter()
            .map(|mod_info| ModSummary {
                mod_unit_id: mod_info.mod_unit_id,
                mod_id: mod_info.mod_id.clone(),
                name: mod_info.name.clone(),
                version: mod_info.version.clone(),
                loader: mod_info.loader.clone().unwrap_or_default(),
                minecraft_version: mod_info.minecraft_version.clone(),
            })
            .collect(),
        namespaces,
        languages,
        nested_archives: metadata
            .nested_archives
            .iter()
            .map(|archive| NestedArchiveSummary {
                mod_unit_id: archive.mod_unit_id,
                archive_path: archive.archive_path.clone(),
                mod_id: archive.mod_id.clone(),
                name: archive.name.clone(),
                version: archive.version.clone(),
                has_languages: archive.has_languages,
                skipped_reason: archive.skipped_reason.clone(),
            })
            .collect(),
        recommended_source_locale: Some(source_locale.clone()),
        source_locale,
        fallback_priority: review.fallback_priority,
        preserve_existing_japanese: review.preserve_existing_japanese,
        fill_missing_from_other_languages: review.fill_missing_from_other_languages,
        included_namespaces,
        setup_completed: review.setup_completed,
        existing_japanese_translation_count: entry_summary.existing_japanese,
        missing_source_keys_count: 0,
        counts: project_counts_from_summary(entry_summary),
        target_language_file_name,
        target_language_format,
        is_signed,
        asset_count: Some(asset_summary.total),
        asset_summary,
        minecraft_version_detection,
        supported_minecraft_versions: yc_export::supported_minecraft_versions(),
    })
}

fn discovered_languages_from_records(languages: &[LanguageRecord]) -> Vec<DiscoveredLanguageFile> {
    languages
        .iter()
        .map(|language| {
            let file_name = Path::new(&language.archive_path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned();
            DiscoveredLanguageFile {
                namespace: language.namespace.clone(),
                locale: language.locale.clone(),
                normalized_locale: normalize_locale(&language.locale),
                format: language.format.clone(),
                container_path: language.container_path.clone(),
                archive_path: language.archive_path.clone(),
                file_name,
            }
        })
        .collect()
}

fn project_counts_from_summary(summary: ProjectEntrySummary) -> ProjectCounts {
    ProjectCounts {
        total: summary.total,
        translated: summary.translated,
        missing: summary.total.saturating_sub(summary.translated),
        needs_review: summary.needs_review,
    }
}

pub(crate) fn detect_minecraft_version_for_project(
    source_jar: &Path,
    metadata_ranges: &[&str],
) -> Option<yc_export::MinecraftVersionDetection> {
    let Ok(mut archive) = yc_archive::Archive::open(source_jar) else {
        tracing::warn!(path = %source_jar.display(), "Minecraft version detection could not open the project JAR");
        return yc_export::detect_minecraft_version(metadata_ranges, None, &[]);
    };
    let paths: Vec<_> = archive
        .list_entries()
        .iter()
        .filter(|entry| !entry.is_directory)
        .filter_map(|entry| entry.normalized_path.as_ref())
        .map(|path| path.to_string())
        .collect();
    let pack_mcmeta = archive
        .read_entry("pack.mcmeta", yc_archive::EntryKind::Text)
        .ok();
    yc_export::detect_minecraft_version(metadata_ranges, pack_mcmeta.as_deref(), &paths)
}

pub(crate) fn review_settings_from_record(
    review: &ProjectReviewSettings,
    analysis: &AnalysisData,
    mod_unit_id: i64,
) -> ReviewSettings {
    let available_namespaces: HashSet<_> = analysis
        .result
        .language_files
        .iter()
        .filter(|language| language.entry_count > 0)
        .map(|language| language.namespace.as_str())
        .chain(
            analysis
                .result
                .patchouli_books
                .iter()
                .map(|book| book.namespace.as_str()),
        )
        .collect();
    ReviewSettings {
        source_locale: resolve_source_locale(
            analysis,
            review.source_locale.as_deref().unwrap_or_default(),
        ),
        fallback_priority: review.fallback_priority.clone(),
        preserve_existing_japanese: review.preserve_existing_japanese,
        fill_missing_from_other_languages: review.fill_missing_from_other_languages,
        // 保存済み選択が言語Entryのない名前空間を指しても、ReviewやExportの対象へ戻さない。
        included_namespaces: review
            .included_namespaces
            .iter()
            .filter(|selection| {
                selection.mod_unit_id == mod_unit_id
                    && available_namespaces.contains(selection.namespace.as_str())
            })
            .map(|selection| selection.namespace.clone())
            .collect(),
    }
}

pub(crate) fn merge_for_review(
    analysis: &AnalysisData,
    settings: &ReviewSettings,
) -> yc_analysis::ReviewMergeResult {
    if settings.included_namespaces.is_empty() {
        // yc-analysisは空配列を「既定namespace」に補うため、存在しない値で空選択を保持する。
        let mut explicit_empty = settings.clone();
        explicit_empty.included_namespaces = vec!["\0yakucraft-no-namespace-selected".to_owned()];
        analysis.merge_review(&explicit_empty)
    } else {
        analysis.merge_review(settings)
    }
}

fn exclude_empty_modpack_entries(entries: &mut Vec<TranslationEntry>) {
    entries.retain(|entry| {
        !entry
            .source_text
            .as_deref()
            .is_some_and(|source| source.trim().is_empty())
    });
}

fn mark_existing_placeholder_errors(entries: &mut [TranslationEntry]) {
    for entry in entries.iter_mut().filter(|entry| {
        entry.origin == TranslationOrigin::Existing && !entry.target_text.is_empty()
    }) {
        if entry.source_text.as_deref().is_some_and(|source| {
            let profile = if entry.flags.contains(&EntryFlag::PatchouliText) {
                PlaceholderProfile::Patchouli
            } else {
                PlaceholderProfile::Lang
            };
            !compare_placeholders_for_profile(source, &entry.target_text, profile).is_valid
        }) && !entry.flags.contains(&EntryFlag::PlaceholderError)
        {
            // 解析時に取り込む既存訳も編集済み訳と同じ基準で警告し、Exportまで不一致を引き継ぐ。
            entry.flags.push(EntryFlag::PlaceholderError);
        }
    }
}

fn preserve_saved_translations(entries: &mut [TranslationEntry], saved_entries: Vec<ProjectEntry>) {
    let saved_by_key: HashMap<_, _> = saved_entries
        .into_iter()
        .filter(|entry| {
            matches!(
                entry.origin,
                TranslationOrigin::Ai | TranslationOrigin::Edited
            )
        })
        .map(|entry| ((entry.namespace.clone(), entry.key.clone()), entry))
        .collect();
    for entry in entries {
        if let Some(saved) = saved_by_key.get(&(entry.namespace.clone(), entry.key.clone())) {
            entry.target_text = saved.target_text.clone().unwrap_or_default();
            entry.target_original_text = saved.target_original_text.clone();
            entry.translated_source_text = saved.translated_source_text.clone();
            entry.origin = saved.origin.clone();
        }
    }
}

fn asset_index_or_warn(
    source_path: &Path,
    entries: &[TranslationEntry],
    project_id: &str,
) -> Option<ProjectAssetIndex> {
    match build_project_asset_index(source_path, entries) {
        Ok(index) => Some(index),
        Err(error) => {
            tracing::warn!(
                project_id = %project_id,
                %error,
                "Project asset indexing failed; translation analysis continues"
            );
            None
        }
    }
}

fn review_asset_index_or_warn(
    source_path: &Path,
    analysis: &AnalysisData,
    settings: &ReviewSettings,
    entries: &[TranslationEntry],
    project_id: &str,
) -> Option<ProjectAssetIndex> {
    match build_review_asset_index(source_path, analysis, settings, entries) {
        Ok(index) => Some(index),
        Err(error) => {
            tracing::warn!(
                project_id = %project_id,
                %error,
                "Review asset indexing failed; translation setup continues"
            );
            None
        }
    }
}

fn save_asset_index_or_warn(
    opened: &OpenedProject,
    mod_unit_id: i64,
    index: Option<ProjectAssetIndex>,
    project_id: &str,
) {
    let Some(index) = index else {
        return;
    };
    if let Err(error) = opened.writer().save_asset_index(mod_unit_id, index) {
        tracing::warn!(
            project_id = %project_id,
            %error,
            "Project asset index could not be saved; translation analysis continues"
        );
    }
}

fn build_project_asset_index(
    source_path: &Path,
    entries: &[TranslationEntry],
) -> Result<ProjectAssetIndex, String> {
    let archive = yc_archive::Archive::open(source_path).map_err(|error| error.to_string())?;
    let paths = archive
        .list_entries()
        .iter()
        .filter(|entry| !entry.is_directory)
        .filter_map(|entry| {
            entry
                .normalized_path
                .as_ref()
                .map(|path| path.as_ref().to_owned())
        })
        .collect();
    drop(archive);

    let resolver = AssetResolver::new(vec![AssetPack::new(
        PROJECT_ASSET_SOURCE_ID,
        SourceKind::Mod,
        Arc::new(PathOnlyAssetSource { paths }),
    )]);
    let mod_source_ids = BTreeSet::from([PROJECT_ASSET_SOURCE_ID.to_owned()]);
    build_project_asset_index_from_resolver(&resolver, entries, &mod_source_ids)
}

fn build_review_asset_index(
    source_path: &Path,
    analysis: &AnalysisData,
    settings: &ReviewSettings,
    entries: &[TranslationEntry],
) -> Result<ProjectAssetIndex, String> {
    let archive_scan_started = Instant::now();
    let mut root_archive =
        yc_archive::Archive::open(source_path).map_err(|error| error.to_string())?;
    let root_paths = root_archive
        .list_entries()
        .iter()
        .filter(|entry| !entry.is_directory)
        .filter_map(|entry| entry.normalized_path.as_ref())
        .map(|path| path.to_string())
        .collect();
    let mut packs = vec![AssetPack::new(
        PROJECT_ASSET_SOURCE_ID,
        SourceKind::Mod,
        Arc::new(PathOnlyAssetSource { paths: root_paths }),
    )];
    let mut mod_source_ids = BTreeSet::from([PROJECT_ASSET_SOURCE_ID.to_owned()]);
    let included_namespaces: BTreeSet<_> = settings
        .included_namespaces
        .iter()
        .map(String::as_str)
        .collect();
    // Previewと同じく、Rootのほか選択Namespaceを含む同梱JARだけを索引へ加える。
    let nested_paths: BTreeSet<_> = analysis
        .result
        .namespaces
        .iter()
        .filter(|namespace| included_namespaces.contains(namespace.namespace.as_str()))
        .map(|namespace| namespace.container_path.as_str())
        .filter(|container_path| !container_path.is_empty())
        .collect();
    let nested_paths: Vec<_> = nested_paths.into_iter().map(str::to_owned).collect();
    if !nested_paths.is_empty() {
        for nested in root_archive
            .discover_nested(&nested_paths)
            .map_err(|error| error.to_string())?
        {
            if !nested_paths.contains(&nested.path) {
                continue;
            }
            let Some(archive) = nested.archive else {
                continue;
            };
            let source_id = format!("{PROJECT_ASSET_SOURCE_ID}:{}", nested.path);
            let paths = archive
                .list_entries()
                .iter()
                .filter(|entry| !entry.is_directory)
                .filter_map(|entry| entry.normalized_path.as_ref())
                .map(|path| path.to_string())
                .collect();
            mod_source_ids.insert(source_id.clone());
            packs.push(AssetPack::new(
                source_id,
                SourceKind::Mod,
                Arc::new(PathOnlyAssetSource { paths }),
            ));
        }
    }
    tracing::info!(
        stage = "review_asset_archive_paths",
        elapsed_ms = archive_scan_started.elapsed().as_millis(),
        archive_count = packs.len(),
        "Review asset phase completed"
    );
    let asset_index_started = Instant::now();
    let resolver = AssetResolver::new(packs);
    tracing::info!(
        stage = "review_asset_path_index",
        elapsed_ms = asset_index_started.elapsed().as_millis(),
        asset_count = resolver.index().assets().len(),
        "Review asset phase completed"
    );
    build_project_asset_index_from_resolver(&resolver, entries, &mod_source_ids)
}

fn build_project_asset_index_from_resolver(
    resolver: &AssetResolver,
    entries: &[TranslationEntry],
    mod_source_ids: &BTreeSet<String>,
) -> Result<ProjectAssetIndex, String> {
    let link_started = Instant::now();
    let assets: Vec<_> = resolver
        .index()
        .assets()
        .iter()
        .filter(|asset| mod_source_ids.contains(&asset.source_id))
        .map(|asset| AssetRecord {
            namespace: asset.namespace.clone(),
            asset_type: asset_type_name(asset.asset_type).to_owned(),
            resource_id: asset.resource_id.clone(),
            // assets.archive_pathは一意制約があるため、同じ内部パスを持つRoot/Nested JARを区別する。
            archive_path: project_asset_storage_key(&asset.source_id, &asset.archive_path),
        })
        .collect();

    let mut matches = Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| is_asset_translation_key(&entry.key))
    {
        let links: Vec<_> = resolver
            .index()
            .links_for_translation_key(&entry.key)
            .into_iter()
            .filter(|link| mod_source_ids.contains(&link.asset.source_id))
            .collect();
        let mut best_confidence_by_type = BTreeMap::<AssetType, u8>::new();
        for link in &links {
            let rank = confidence_rank(link.confidence);
            best_confidence_by_type
                .entry(link.asset.asset_type)
                .and_modify(|best| *best = (*best).min(rank))
                .or_insert(rank);
        }
        let has_tied_candidates = best_confidence_by_type
            .iter()
            .any(|(asset_type, best_rank)| {
                links
                    .iter()
                    .filter(|link| {
                        link.asset.asset_type == *asset_type
                            && confidence_rank(link.confidence) == *best_rank
                    })
                    .count()
                    > 1
            });
        let has_confirmed_candidate = links.iter().any(|link| link.confirmed);
        let missing = links.is_empty();
        let ambiguous = !missing && (!has_confirmed_candidate || has_tied_candidates);
        let primary = (!ambiguous)
            .then(|| links.iter().find(|link| link.confirmed))
            .flatten();

        // Candidate links are needed only while deciding the primary match, missing state, and
        // ambiguity. Preview reopens the source JAR, so retaining every candidate path per key
        // duplicates a large index in memory without affecting later reads.
        matches.push(TranslationAssetMatch {
            namespace: entry.namespace.clone(),
            key: entry.key.clone(),
            asset_kind: primary.map(|link| asset_type_name(link.asset.asset_type).to_owned()),
            asset_id: primary.map(|link| link.asset.resource_id.clone()),
            confidence: primary.map(|link| confidence_name(link.confidence).to_owned()),
            missing,
            ambiguous,
        });
    }

    tracing::info!(
        stage = "review_asset_key_linking",
        elapsed_ms = link_started.elapsed().as_millis(),
        entry_count = entries.len(),
        match_count = matches.len(),
        "Review asset phase completed"
    );
    Ok(ProjectAssetIndex { assets, matches })
}

fn project_asset_storage_key(source_id: &str, archive_path: &str) -> String {
    format!("{}:{source_id}{archive_path}", source_id.len())
}

fn is_asset_translation_key(key: &str) -> bool {
    let mut parts = key.splitn(3, '.');
    matches!(parts.next(), Some("block" | "item" | "entity"))
        && parts.next().is_some_and(|namespace| !namespace.is_empty())
        && parts
            .next()
            .is_some_and(|resource_id| !resource_id.is_empty())
}

fn asset_type_name(asset_type: AssetType) -> &'static str {
    match asset_type {
        AssetType::Blockstate => "blockstate",
        AssetType::BlockModel => "block_model",
        AssetType::ItemModel => "item_model",
        AssetType::ItemDefinition => "item_definition",
        AssetType::TextureBlock => "texture_block",
        AssetType::TextureItem => "texture_item",
        AssetType::TextureEntity => "texture_entity",
        AssetType::GeoModel => "geo_model",
        AssetType::Animation => "animation",
        AssetType::OtherModel => "other_model",
    }
}

fn confidence_rank(confidence: LinkConfidence) -> u8 {
    match confidence {
        LinkConfidence::High => 0,
        LinkConfidence::Medium => 1,
        LinkConfidence::Low => 2,
    }
}

fn confidence_name(confidence: LinkConfidence) -> &'static str {
    match confidence {
        LinkConfidence::High => "high",
        LinkConfidence::Medium => "medium",
        LinkConfidence::Low => "low",
    }
}

struct PathOnlyAssetSource {
    paths: Vec<String>,
}

impl AssetSource for PathOnlyAssetSource {
    fn paths(&self) -> Vec<String> {
        self.paths.clone()
    }

    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        Err(format!(
            "Asset content is not read during Review indexing: {path}"
        ))
    }
}

fn project_asset_summary(
    asset_type_counts: &HashMap<String, usize>,
    entries: &[ProjectEntry],
) -> ProjectAssetSummary {
    let count = |asset_type: &str| {
        asset_type_counts
            .get(asset_type)
            .copied()
            .unwrap_or_default()
    };
    ProjectAssetSummary {
        total: asset_type_counts.values().sum(),
        block_models: count("block_model"),
        item_models: count("item_model"),
        entity_textures: count("texture_entity"),
        geometry_models: count("geo_model"),
        unresolved_entries: entries
            .iter()
            .filter(|entry| {
                entry.flags.contains(&EntryFlag::AssetMissing)
                    || entry.flags.contains(&EntryFlag::AssetAmbiguous)
            })
            .count(),
    }
}

pub(crate) fn discovered_languages(analysis: &AnalysisData) -> Vec<DiscoveredLanguageFile> {
    analysis
        .result
        .language_files
        .iter()
        .map(|language| {
            let archive_path = Path::new(&language.archive_path);
            DiscoveredLanguageFile {
                namespace: language.namespace.clone(),
                locale: language.locale.clone(),
                normalized_locale: normalize_locale(&language.locale),
                format: language.format.clone(),
                container_path: language.container_path.clone(),
                archive_path: language.archive_path.clone(),
                file_name: archive_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_owned(),
            }
        })
        .collect()
}

pub(crate) fn project_analysis_data(
    analysis: &AnalysisData,
    entries: Vec<TranslationEntry>,
    settings: &ReviewSettings,
) -> ProjectAnalysisData {
    let mut entry_counts = analysis.result.language_files.iter().fold(
        HashMap::<String, usize>::new(),
        |mut counts, language| {
            *counts.entry(language.namespace.clone()).or_default() += language.entry_count;
            counts
        },
    );
    for entry in analysis
        .patchouli_entries
        .iter()
        .filter(|entry| settings.included_namespaces.contains(&entry.namespace))
    {
        *entry_counts.entry(entry.namespace.clone()).or_default() += 1;
    }
    ProjectAnalysisData {
        mods: analysis
            .result
            .mods
            .iter()
            .map(|mod_info| ModRecord {
                mod_unit_id: 0,
                mod_id: mod_info.mod_id.clone(),
                name: mod_info.name.clone(),
                version: mod_info.version.clone(),
                loader: Some(format!("{:?}", mod_info.loader)),
                minecraft_version: mod_info
                    .minecraft_version_estimate
                    .clone()
                    .or_else(|| mod_info.minecraft_version.clone()),
                metadata_source: mod_info.metadata_source.clone(),
            })
            .collect(),
        namespaces: analysis
            .result
            .namespaces
            .iter()
            .map(|namespace| NamespaceRecord {
                mod_unit_id: 0,
                namespace: namespace.namespace.clone(),
                container_path: namespace.container_path.clone(),
                included: settings.included_namespaces.contains(&namespace.namespace),
                entry_count: entry_counts
                    .get(&namespace.namespace)
                    .copied()
                    .unwrap_or_default(),
            })
            .collect(),
        nested_archives: analysis
            .result
            .nested_archives
            .iter()
            .map(|archive| NestedArchiveRecord {
                mod_unit_id: 0,
                archive_path: archive.archive_path.clone(),
                mod_id: archive.mod_id.clone(),
                name: archive.name.clone(),
                version: archive.version.clone(),
                has_languages: archive.has_languages,
                skipped_reason: archive.skipped_reason.clone(),
            })
            .collect(),
        languages: analysis
            .result
            .language_files
            .iter()
            .map(|language| LanguageRecord {
                mod_unit_id: 0,
                namespace: language.namespace.clone(),
                locale: language.locale.clone(),
                format: language.format.clone(),
                container_path: language.container_path.clone(),
                archive_path: language.archive_path.clone(),
                entry_count: language.entry_count,
                is_target: normalize_locale(&language.locale) == "ja_jp",
            })
            .collect(),
        entries,
    }
}

fn document_entries_for_analysis(
    analysis: &AnalysisData,
    settings: &ReviewSettings,
) -> Vec<DocumentEntryInput> {
    analysis
        .patchouli_entries
        .iter()
        .filter(|entry| settings.included_namespaces.contains(&entry.namespace))
        .enumerate()
        .map(|(sort_index, entry)| {
            let target_text = settings
                .preserve_existing_japanese
                .then(|| entry.target_text.clone())
                .flatten();
            let existing = target_text.is_some();
            let mut flags = Vec::new();
            if existing
                && !compare_placeholders_for_profile(
                    &entry.source_text,
                    entry.target_text.as_deref().unwrap_or_default(),
                    PlaceholderProfile::Patchouli,
                )
                .is_valid
            {
                flags.push(EntryFlag::PlaceholderError);
            }
            DocumentEntryInput {
                namespace: entry.namespace.clone(),
                key: entry.key.clone(),
                entry_kind: yc_project::TranslationEntryKind::Patchouli,
                document_path: entry.document_path.clone(),
                document_pointer: entry.pointer.clone(),
                source_text: Some(entry.source_text.clone()),
                source_locale: Some("en_us".to_owned()),
                target_text: target_text.clone(),
                target_original_text: target_text,
                translated_source_text: existing.then(|| entry.source_text.clone()),
                origin: if existing {
                    TranslationOrigin::Existing
                } else {
                    TranslationOrigin::Missing
                },
                sort_index,
                flags,
            }
        })
        .collect()
}

fn modpack_document_entries(
    data: &yc_modpack::ModpackData,
    settings: &ReviewSettings,
) -> Vec<DocumentEntryInput> {
    data.entries
        .iter()
        .filter(|entry| entry.translatable && entry.key.starts_with("ftbquests:"))
        .enumerate()
        .map(|(sort_index, entry)| {
            let target_text = settings
                .preserve_existing_japanese
                .then(|| entry.existing_translation.clone())
                .flatten();
            let existing = target_text.is_some();
            let mut flags = Vec::new();
            if existing
                && !compare_placeholders_for_profile(
                    &entry.source_text,
                    target_text.as_deref().unwrap_or_default(),
                    PlaceholderProfile::FtbQuests,
                )
                .is_valid
            {
                flags.push(EntryFlag::PlaceholderError);
            }
            DocumentEntryInput {
                namespace: "ftbquests".to_owned(),
                key: entry.key.clone(),
                entry_kind: yc_project::TranslationEntryKind::Ftbquests,
                document_path: entry.document_path.clone(),
                document_pointer: entry.pointer.clone(),
                source_text: Some(entry.source_text.clone()),
                source_locale: Some("en_us".to_owned()),
                target_text: target_text.clone(),
                target_original_text: target_text,
                translated_source_text: existing.then(|| entry.source_text.clone()),
                origin: if existing {
                    TranslationOrigin::Existing
                } else {
                    TranslationOrigin::Missing
                },
                sort_index,
                flags,
            }
        })
        .collect()
}

pub(crate) fn touch_recent_project(
    paths: &AppPaths,
    summary: &ProjectSummary,
    mod_version: Option<String>,
) -> Result<(), AppError> {
    let limit = recent_projects_limit(paths)?;
    let mut recent = load_recent_projects(&paths.recent_file)
        .map_err(|error| database_error(error.to_string()))?;
    recent.retain_existing(&paths.projects_dir);
    let repository =
        yc_project::Repository::new(paths.projects_dir.join(&summary.id).join("project.sqlite"));
    let progress = project_counts_from_summary(
        repository
            .entry_summary(None)
            .map_err(|error| database_error(error.to_string()))?,
    );
    let last_opened = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| database_error(error.to_string()))?;
    let old_version = recent
        .projects
        .iter()
        .find(|project| project.id == summary.id)
        .and_then(|project| project.mod_version.clone());
    recent.projects.retain(|project| project.id != summary.id);
    recent.projects.push(RecentProject {
        id: summary.id.clone(),
        display_name: summary.display_name.clone(),
        mod_version: mod_version.or(old_version),
        mod_count: summary.mod_count,
        instance_path: summary.instance_path.clone(),
        last_opened,
        translation_progress: TranslationProgress {
            translated: progress.translated,
            missing: progress.missing,
            needs_review: progress.needs_review,
        },
    });
    recent
        .projects
        .sort_by(|left, right| right.last_opened.cmp(&left.last_opened));
    truncate_recent_projects(&mut recent, limit);
    save_recent_projects(&paths.recent_file, &recent)
        .map_err(|error| database_error(error.to_string()))
}

pub(crate) fn refresh_recent_project_progress(
    paths: &AppPaths,
    summary: &ProjectSummary,
) -> Result<(), AppError> {
    let limit = recent_projects_limit(paths)?;
    let mut recent = load_recent_projects(&paths.recent_file)
        .map_err(|error| database_error(error.to_string()))?;
    recent.retain_existing(&paths.projects_dir);
    let repository =
        yc_project::Repository::new(paths.projects_dir.join(&summary.id).join("project.sqlite"));
    let progress = project_counts_from_summary(
        repository
            .entry_summary(None)
            .map_err(|error| database_error(error.to_string()))?,
    );
    if let Some(project) = recent
        .projects
        .iter_mut()
        .find(|item| item.id == summary.id)
    {
        project.display_name.clone_from(&summary.display_name);
        project.mod_count = summary.mod_count;
        project.instance_path.clone_from(&summary.instance_path);
        project.translation_progress = TranslationProgress {
            translated: progress.translated,
            missing: progress.missing,
            needs_review: progress.needs_review,
        };
        truncate_recent_projects(&mut recent, limit);
        save_recent_projects(&paths.recent_file, &recent)
            .map_err(|error| database_error(error.to_string()))
    } else {
        touch_recent_project(paths, summary, None)
    }
}

pub(crate) fn list_recent_projects_core(paths: &AppPaths) -> Result<Vec<RecentProject>, AppError> {
    let limit = recent_projects_limit(paths)?;
    let available =
        list_projects(&paths.projects_dir).map_err(|error| database_error(error.to_string()))?;
    let valid_ids: std::collections::HashSet<_> = available
        .iter()
        .map(|summary| summary.id.as_str())
        .collect();
    let mut recent = load_recent_projects(&paths.recent_file)
        .map_err(|error| database_error(error.to_string()))?;
    recent.retain_existing(&paths.projects_dir);
    recent
        .projects
        .retain(|project| valid_ids.contains(project.id.as_str()));
    for summary in available {
        if recent
            .projects
            .iter()
            .any(|project| project.id == summary.id)
        {
            continue;
        }
        recent.projects.push(RecentProject {
            id: summary.id.clone(),
            display_name: summary.display_name.clone(),
            mod_version: None,
            mod_count: summary.mod_count,
            instance_path: summary.instance_path.clone(),
            last_opened: summary.updated_at.clone(),
            translation_progress: TranslationProgress::default(),
        });
    }
    for project in &mut recent.projects {
        let repository = yc_project::Repository::new(
            paths.projects_dir.join(&project.id).join("project.sqlite"),
        );
        if let Ok(summary) = repository.entry_summary(None) {
            let counts = project_counts_from_summary(summary);
            project.translation_progress = TranslationProgress {
                translated: counts.translated,
                missing: counts.missing,
                needs_review: counts.needs_review,
            };
        }
    }
    recent
        .projects
        .sort_by(|left, right| right.last_opened.cmp(&left.last_opened));
    truncate_recent_projects(&mut recent, limit);
    save_recent_projects(&paths.recent_file, &recent)
        .map_err(|error| database_error(error.to_string()))?;
    Ok(recent.projects)
}

pub(crate) fn trim_recent_projects_to_setting(paths: &AppPaths) -> Result<(), AppError> {
    let limit = recent_projects_limit(paths)?;
    let mut recent = load_recent_projects(&paths.recent_file)
        .map_err(|error| database_error(error.to_string()))?;
    truncate_recent_projects(&mut recent, limit);
    save_recent_projects(&paths.recent_file, &recent)
        .map_err(|error| database_error(error.to_string()))
}

fn recent_projects_limit(paths: &AppPaths) -> Result<usize, AppError> {
    let settings =
        load_settings(&paths.settings_file).map_err(|error| database_error(error.to_string()))?;
    Ok(settings
        .values
        .get("general.recentProjectsCount")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(10)
        .clamp(1, 50) as usize)
}

fn truncate_recent_projects(recent: &mut RecentProjects, limit: usize) {
    recent
        .projects
        .sort_by(|left, right| right.last_opened.cmp(&left.last_opened));
    recent.projects.truncate(limit);
}

fn format_label(format: &LanguageFileFormat) -> &'static str {
    match format {
        LanguageFileFormat::Json => "json",
        LanguageFileFormat::LegacyLang => "legacyLang",
    }
}

fn modpack_refresh_io_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        "modpack_refresh",
        "Modpack のファイルを再取り込みできませんでした。",
        Some(error.to_string()),
    )
}

fn analysis_error(error: yc_analysis::AnalysisError) -> AppError {
    match error {
        yc_analysis::AnalysisError::Cancelled => cancelled_error(),
        yc_analysis::AnalysisError::NoLanguageFiles => AppError::new(
            "no_language_files",
            "このJARから言語ファイルを見つけられませんでした。assets/*/lang を含むJARを選んでください。",
            Some(error.to_string()),
        ),
        other => AppError::new(
            "archive_read",
            "JARを読み込めませんでした。ファイルが破損していないか確認してください。",
            Some(other.to_string()),
        ),
    }
}

fn database_error(detail: String) -> AppError {
    AppError::new(
        "database",
        "プロジェクトを読み込めませんでした。アプリを再起動してもう一度お試しください。",
        Some(detail),
    )
}

fn cancelled_error() -> AppError {
    AppError::simple(
        "cancelled",
        "処理を中止しました。必要であればもう一度お試しください。",
    )
}

fn task_error(detail: String) -> AppError {
    AppError::new(
        "task_failed",
        "処理に失敗しました。もう一度お試しください。",
        Some(detail),
    )
}

fn internal_state_error(label: &str) -> AppError {
    AppError::new(
        "internal_state",
        "アプリの状態を読み取れませんでした。再起動してもう一度お試しください。",
        Some(label.to_owned()),
    )
}

fn cleanup_project(paths: &AppPaths, project_id: &str) {
    if let Ok(id) = Uuid::parse_str(project_id)
        && let Err(error) = remove_project(&paths.projects_dir, id)
    {
        tracing::warn!(project_id, %error, "incomplete Project cleanup failed");
    }
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
