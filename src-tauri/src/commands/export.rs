use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use yc_analysis::CancellationToken;
use yc_export::{
    ExportError, ExportInput, ExportKind, ExportValidationRequest, ManualPackFormat, MergedPackMod,
    MergedResourcePackConflict, MergedResourcePackOptions, NamespaceExport,
    PatchouliDocumentExport, PatchouliDocumentTranslation, PlaceholderWarning, ResourcePackMod,
    ResourcePackOptions, ValidationIssueCode, ValidationSeverity, export_merged_resource_pack,
    export_modified_jar as write_modified_jar, export_resource_pack as write_resource_pack,
    install_resource_pack_to, merged_resource_pack_display_name, merged_resource_pack_filename,
    replace_jar_in_place, replace_jar_in_place_with_backup, restore_jar_from_backup,
    validate_export as check_export,
};
use yc_language::{
    EntryFlag, ReviewSettings, TranslationEntry, WriterOptions, is_empty_translation,
    normalize_locale,
};
use yc_project::{
    DocumentProjectEntry, FileDeploymentRecord, JarReplacementRecord, ModUnitRecord,
    ModUnitSourceKind, OpenedProject, ProjectEntry, TranslationEntryKind,
};
use yc_validation::{PlaceholderProfile, PlaceholderSeverity, compare_placeholders_for_profile};

use crate::{
    commands::project::{
        detect_minecraft_version_for_project, discovered_languages, merge_for_review,
        project_review_settings_for_unit, review_settings_from_record, shaderpack_namespace,
    },
    error::AppError,
    state::AppState,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportKindInput {
    LanguageFile,
    ResourcePack,
    ModifiedJar,
    ModpackFiles,
    ShaderpackZip,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportIssue {
    pub severity: String,
    pub code: String,
    pub message: String,
    pub mod_unit_id: Option<i64>,
    pub mod_name: Option<String>,
    pub count: Option<usize>,
    pub namespace: Option<String>,
    pub key: Option<String>,
    pub display_label: Option<String>,
    pub source_text: Option<String>,
    pub target_text: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportValidationResult {
    pub issues: Vec<ExportIssue>,
    pub counts: crate::commands::project::ProjectCounts,
    pub is_signed: bool,
    pub minecraft_version_detection: Option<yc_export::MinecraftVersionDetection>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportUnitStatus {
    pub id: i64,
    pub source_kind: ModUnitSourceKind,
    pub source_filename: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub source_sha256: String,
    pub original_path: String,
    pub original_exists: bool,
    pub in_instance_mods: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOverview {
    pub instance_path: Option<String>,
    pub units: Vec<ExportUnitStatus>,
    pub active_replacements: Vec<JarReplacementRecord>,
    pub active_file_deployments: Vec<FileDeploymentRecord>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModExportResult {
    pub mod_unit_id: i64,
    pub mod_name: String,
    pub status: String,
    pub output_path: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
// フロントエンドDTOもcamelCaseのため、variant名だけでなくvariant内のフィールド名も変換する。
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum MergedPackExportOutcome {
    OverwriteRequired {
        output_path: String,
    },
    Completed {
        output_path: String,
        directly_installed: bool,
        conflicts: Vec<MergedResourcePackConflict>,
        mod_results: Vec<ModExportResult>,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RestoreJarOutcome {
    ConfirmationRequired { target_path: String },
    Restored { target_path: String },
}

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ModpackFilesExportOutcome {
    OverwriteRequired {
        target_path: String,
    },
    Completed {
        output_path: String,
        directly_installed: bool,
        file_count: usize,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ShaderpackExportOutcome {
    OverwriteRequired {
        target_paths: Vec<String>,
    },
    Completed {
        output_paths: Vec<String>,
        directly_installed: bool,
        mod_results: Vec<ModExportResult>,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RestoreFileDeploymentOutcome {
    Restored { target_path: String },
}

struct MergedPackExportRequest<'a> {
    selected_ids: &'a [i64],
    requested_output: Option<&'a Path>,
    minecraft_version: &'a str,
    manual_format: Option<ManualPackFormatInput>,
    direct_install: bool,
    overwrite: bool,
}

fn active_project_for_id<'a>(
    active: &'a Option<OpenedProject>,
    project_id: &str,
) -> Result<&'a OpenedProject, AppError> {
    let opened = active
        .as_ref()
        .ok_or_else(|| project_error("開いているプロジェクトがありません"))?;
    ensure_project_id_matches(&opened.summary().id, project_id)?;
    Ok(opened)
}

fn ensure_project_id_matches(
    active_project_id: &str,
    requested_project_id: &str,
) -> Result<(), AppError> {
    // 画面側がダイアログ待ちの間にProjectが切り替わる場合があるため、active projectのロック内で照合し、
    // 古い要求が切替後のProjectを書き出さないようにする。
    if active_project_id != requested_project_id {
        return Err(AppError::simple(
            "project_changed",
            "プロジェクトが切り替わりました。",
        ));
    }
    Ok(())
}

struct ExportContext {
    mod_unit_id: i64,
    source_kind: ModUnitSourceKind,
    mod_name: String,
    source_jar: PathBuf,
    source_jar_filename: String,
    mods: Vec<ResourcePackMod>,
    entries: Vec<TranslationEntry>,
    document_entries: Vec<DocumentProjectEntry>,
    namespaces: Vec<NamespaceExport>,
    language_files: Vec<yc_language::DiscoveredLanguageFile>,
    legacy_parse_escapes_namespaces: Vec<String>,
    placeholder_warnings: Vec<PlaceholderWarning>,
    patchouli_documents: Vec<PatchouliDocumentExport>,
    counts: crate::commands::project::ProjectCounts,
    malformed_translation_files: Vec<String>,
    duplicate_key_warnings: Vec<(String, String)>,
    source_locale: String,
    is_signed: bool,
    minecraft_version_detection: Option<yc_export::MinecraftVersionDetection>,
}

impl ExportContext {
    fn input(&self) -> ExportInput<'_> {
        ExportInput {
            entries: &self.entries,
            namespaces: &self.namespaces,
            language_files: &self.language_files,
            legacy_parse_escapes_namespaces: &self.legacy_parse_escapes_namespaces,
            source_locale: &self.source_locale,
            writer_options: WriterOptions::default(),
            malformed_translation_files: &self.malformed_translation_files,
            placeholder_warnings: &self.placeholder_warnings,
            patchouli_documents: &self.patchouli_documents,
        }
    }
}

#[tauri::command]
pub async fn validate_project(
    kind: ExportKindInput,
    minecraft_version: Option<String>,
    namespace: Option<String>,
    manual_format: Option<ManualPackFormatInput>,
    state: State<'_, AppState>,
) -> Result<ExportValidationResult, AppError> {
    let active_project = Arc::clone(&state.active_project);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| project_error("開いているプロジェクトがありません"))?;
        validate_project_core(
            opened,
            kind,
            minecraft_version.as_deref(),
            namespace.as_deref(),
            manual_format,
        )
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn export_language_file(
    project_id: String,
    path: String,
    namespace: String,
    state: State<'_, AppState>,
) -> Result<String, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let exported_paths = Arc::clone(&state.exported_paths);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active_project_for_id(&active, &project_id)?;
        let output =
            export_language_file_and_record(opened, Path::new(&path), &namespace, &exported_paths)?;
        Ok(output.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

fn export_language_file_and_record(
    opened: &OpenedProject,
    requested_output: &Path,
    namespace: &str,
    exported_paths: &Arc<std::sync::Mutex<HashSet<PathBuf>>>,
) -> Result<PathBuf, AppError> {
    // 元言語ファイルに合わせて拡張子が補正されるため、記録と結果表示にも実際の出力先を使う。
    let output = export_language_file_core(opened, requested_output, namespace)?;
    record_exported_path(exported_paths, &output)?;
    Ok(output)
}

#[tauri::command]
pub async fn export_resource_pack(
    path: String,
    minecraft_version: String,
    manual_format: Option<ManualPackFormatInput>,
    state: State<'_, AppState>,
) -> Result<String, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let exported_paths = Arc::clone(&state.exported_paths);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| project_error("開いているプロジェクトがありません"))?;
        let output = Path::new(&path);
        export_resource_pack_core(opened, output, &minecraft_version, manual_format)?;
        record_exported_path(&exported_paths, output)?;
        Ok(path)
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn export_modified_jar(
    path: String,
    state: State<'_, AppState>,
) -> Result<String, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let exported_paths = Arc::clone(&state.exported_paths);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active
            .as_ref()
            .ok_or_else(|| project_error("開いているプロジェクトがありません"))?;
        let output = Path::new(&path);
        let result = export_modified_jar_core(opened, output)?;
        record_exported_path(&exported_paths, &result)?;
        Ok(result.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn get_export_overview(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<ExportOverview, AppError> {
    let active_project = Arc::clone(&state.active_project);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active_project_for_id(&active, &project_id)?;
        let instance_path = opened.summary().instance_path.as_deref().map(Path::new);
        let units = opened
            .repository()
            .list_mod_units()
            .map_err(|error| project_error(error.to_string()))?
            .into_iter()
            .map(|unit| {
                let in_instance_mods = unit.source_kind == ModUnitSourceKind::Jar
                    && instance_path.is_some_and(|instance_path| {
                        original_jar_is_in_instance_mods(
                            instance_path,
                            Path::new(&unit.original_path),
                        )
                    });
                ExportUnitStatus {
                    id: unit.id,
                    source_kind: unit.source_kind,
                    source_filename: unit.source_filename,
                    name: unit.name,
                    version: unit.version,
                    source_sha256: unit.source_sha256,
                    original_exists: Path::new(&unit.original_path).is_file(),
                    original_path: unit.original_path,
                    in_instance_mods,
                }
            })
            .collect();
        let active_replacements = opened
            .repository()
            .list_jar_replacements(false)
            .map_err(|error| project_error(error.to_string()))?;
        let active_file_deployments = opened
            .repository()
            .list_file_deployments(false)
            .map_err(|error| project_error(error.to_string()))?;
        Ok(ExportOverview {
            instance_path: opened.summary().instance_path.clone(),
            units,
            active_replacements,
            active_file_deployments,
        })
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn validate_selected_project(
    project_id: String,
    kind: ExportKindInput,
    mod_unit_ids: Vec<i64>,
    minecraft_version: Option<String>,
    namespace: Option<String>,
    manual_format: Option<ManualPackFormatInput>,
    state: State<'_, AppState>,
) -> Result<ExportValidationResult, AppError> {
    let active_project = Arc::clone(&state.active_project);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active_project_for_id(&active, &project_id)?;
        validate_project_core_for_units(
            opened,
            kind,
            minecraft_version.as_deref(),
            namespace.as_deref(),
            manual_format,
            Some(&mod_unit_ids),
        )
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

// Tauri commandの名前付きIPC引数に合わせたシグネチャを維持する。
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn export_selected_merged_resource_pack(
    project_id: String,
    mod_unit_ids: Vec<i64>,
    output_path: Option<String>,
    minecraft_version: String,
    manual_format: Option<ManualPackFormatInput>,
    direct_install: bool,
    overwrite: bool,
    state: State<'_, AppState>,
) -> Result<MergedPackExportOutcome, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let exported_paths = Arc::clone(&state.exported_paths);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active_project_for_id(&active, &project_id)?;
        export_selected_merged_resource_pack_core(
            opened,
            MergedPackExportRequest {
                selected_ids: &mod_unit_ids,
                requested_output: output_path.as_deref().map(Path::new),
                minecraft_version: &minecraft_version,
                manual_format,
                direct_install,
                overwrite,
            },
            &exported_paths,
        )
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn export_selected_modified_jars(
    project_id: String,
    mod_unit_ids: Vec<i64>,
    output_directory: String,
    replace_in_place: bool,
    state: State<'_, AppState>,
) -> Result<Vec<ModExportResult>, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let exported_paths = Arc::clone(&state.exported_paths);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active_project_for_id(&active, &project_id)?;
        export_selected_modified_jars_core(
            opened,
            &mod_unit_ids,
            Path::new(&output_directory),
            replace_in_place,
            &exported_paths,
        )
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn export_selected_language_files(
    project_id: String,
    mod_unit_ids: Vec<i64>,
    output_directory: String,
    namespace: String,
    state: State<'_, AppState>,
) -> Result<Vec<ModExportResult>, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let exported_paths = Arc::clone(&state.exported_paths);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active_project_for_id(&active, &project_id)?;
        export_selected_language_files_core(
            opened,
            &mod_unit_ids,
            Path::new(&output_directory),
            &namespace,
            &exported_paths,
        )
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn export_modpack_files(
    project_id: String,
    mod_unit_ids: Vec<i64>,
    output_path: Option<String>,
    direct_install: bool,
    overwrite: bool,
    state: State<'_, AppState>,
) -> Result<ModpackFilesExportOutcome, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let exported_paths = Arc::clone(&state.exported_paths);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active_project_for_id(&active, &project_id)?;
        export_modpack_files_core(
            opened,
            &mod_unit_ids,
            output_path.as_deref().map(Path::new),
            direct_install,
            overwrite,
            &exported_paths,
        )
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn export_shaderpacks(
    project_id: String,
    mod_unit_ids: Vec<i64>,
    output_directory: Option<String>,
    direct_install: bool,
    overwrite: bool,
    state: State<'_, AppState>,
) -> Result<ShaderpackExportOutcome, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let exported_paths = Arc::clone(&state.exported_paths);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active_project_for_id(&active, &project_id)?;
        export_selected_shaderpacks_core(
            opened,
            &mod_unit_ids,
            output_directory.as_deref().map(Path::new),
            direct_install,
            overwrite,
            &exported_paths,
        )
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn restore_modpack_file(
    project_id: String,
    deployment_id: i64,
    state: State<'_, AppState>,
) -> Result<RestoreFileDeploymentOutcome, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let exported_paths = Arc::clone(&state.exported_paths);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active_project_for_id(&active, &project_id)?;
        restore_modpack_file_core(opened, deployment_id, &exported_paths)
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub async fn restore_modified_jar(
    project_id: String,
    replacement_id: i64,
    confirm_changed: bool,
    state: State<'_, AppState>,
) -> Result<RestoreJarOutcome, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let exported_paths = Arc::clone(&state.exported_paths);
    tauri::async_runtime::spawn_blocking(move || {
        let active = active_project
            .lock()
            .map_err(|_| state_error("active Project state"))?;
        let opened = active_project_for_id(&active, &project_id)?;
        restore_modified_jar_core(opened, replacement_id, confirm_changed, &exported_paths)
    })
    .await
    .map_err(|error| task_error(error.to_string()))?
}

#[tauri::command]
pub fn open_output_folder(
    app: AppHandle,
    path: String,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let requested = fs::canonicalize(&path).map_err(export_error)?;
    // Explorer起動を過去にこのアプリが書き出したファイルに限定し、任意パスを開くIPCを防ぐ。
    let exported = state
        .exported_paths
        .lock()
        .map_err(|_| state_error("exported paths state"))?;
    if !exported.contains(&requested) {
        return Err(AppError::simple(
            "invalid_output_path",
            "このファイルの保存先は開けません。",
        ));
    }
    let folder = requested
        .parent()
        .ok_or_else(|| AppError::simple("invalid_output_path", "保存先フォルダーを開けません。"))?;
    app.opener()
        .open_path(folder.to_string_lossy().into_owned(), None::<&str>)
        .map_err(export_error)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualPackFormatInput {
    pub major: u32,
    pub minor: u32,
}

pub(crate) fn export_resource_pack_core(
    opened: &OpenedProject,
    output: &Path,
    minecraft_version: &str,
    manual_format: Option<ManualPackFormatInput>,
) -> Result<PathBuf, AppError> {
    let units = opened
        .repository()
        .list_mod_units()
        .map_err(|error| project_error(error.to_string()))?;
    let mut first_output = None;
    for unit in &units {
        let context = export_context(opened, unit.id)?;
        if !context
            .namespaces
            .iter()
            .any(|namespace| namespace.included)
        {
            continue;
        }
        let unit_output = output_path_for_mod(output, unit.id, units.len());
        export_resource_pack_for_context(&context, &unit_output, minecraft_version, manual_format)?;
        first_output.get_or_insert(unit_output);
    }
    first_output.ok_or_else(invalid_export_selection_error)
}

fn export_resource_pack_for_context(
    context: &ExportContext,
    output: &Path,
    minecraft_version: &str,
    manual_format: Option<ManualPackFormatInput>,
) -> Result<(), AppError> {
    ensure_output_is_safe(&context.source_jar, output)?;
    ensure_file_extension(output, "zip")?;
    ensure_has_included_namespaces(context)?;
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staging = parent.join(format!(".yakucraft-export-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&staging).map_err(export_error)?;
    let result = (|| {
        let generated = write_resource_pack(
            &context.input(),
            ResourcePackOptions {
                mods: &context.mods,
                source_jar_filename: &context.source_jar_filename,
                minecraft_version: Some(minecraft_version),
                manual_format: manual_format.map(|format| ManualPackFormat {
                    major: format.major,
                    minor: format.minor,
                }),
            },
            &staging,
            false,
        )
        .map_err(export_error)?;
        copy_atomic_selected(&generated, output).map_err(export_error)?;
        Ok::<(), AppError>(())
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

pub(crate) fn export_modified_jar_core(
    opened: &OpenedProject,
    output: &Path,
) -> Result<PathBuf, AppError> {
    let units = opened
        .repository()
        .list_mod_units()
        .map_err(|error| project_error(error.to_string()))?;
    let mut first_output = None;
    for unit in units
        .iter()
        .filter(|unit| unit.source_kind == ModUnitSourceKind::Jar)
    {
        let context = export_context(opened, unit.id)?;
        if !context
            .namespaces
            .iter()
            .any(|namespace| namespace.included)
        {
            continue;
        }
        let unit_output = output_path_for_mod(output, unit.id, units.len());
        ensure_output_is_safe(&context.source_jar, &unit_output)?;
        ensure_file_extension(&unit_output, "jar")?;
        ensure_has_included_namespaces(&context)?;
        write_modified_jar(&context.input(), &context.source_jar, &unit_output, true)
            .map_err(export_error)?;
        first_output.get_or_insert(unit_output);
    }
    if units
        .iter()
        .all(|unit| unit.source_kind == ModUnitSourceKind::Modpack)
    {
        return Err(AppError::simple(
            "jar_unit_required",
            "Mod JAR を選んでください。",
        ));
    }
    first_output.ok_or_else(invalid_export_selection_error)
}

fn invalid_export_selection_error() -> AppError {
    AppError::simple(
        "invalid_export_selection",
        "翻訳対象のNamespaceを選んでください。",
    )
}

fn output_path_for_mod(output: &Path, mod_unit_id: i64, mod_count: usize) -> PathBuf {
    if mod_count <= 1 {
        return output.to_path_buf();
    }
    let stem = output
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("export");
    let extension = output
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    let filename = if extension.is_empty() {
        format!("{stem}-{mod_unit_id}")
    } else {
        format!("{stem}-{mod_unit_id}.{extension}")
    };
    output.with_file_name(filename)
}

pub(crate) fn validate_project_core(
    opened: &OpenedProject,
    kind: ExportKindInput,
    minecraft_version: Option<&str>,
    namespace: Option<&str>,
    manual_format: Option<ManualPackFormatInput>,
) -> Result<ExportValidationResult, AppError> {
    validate_project_core_for_units(
        opened,
        kind,
        minecraft_version,
        namespace,
        manual_format,
        None,
    )
}

fn validate_project_core_for_units(
    opened: &OpenedProject,
    kind: ExportKindInput,
    minecraft_version: Option<&str>,
    namespace: Option<&str>,
    manual_format: Option<ManualPackFormatInput>,
    selected_mod_unit_ids: Option<&[i64]>,
) -> Result<ExportValidationResult, AppError> {
    let units = opened
        .repository()
        .list_mod_units()
        .map_err(|error| project_error(error.to_string()))?;
    if units.is_empty() {
        return Err(project_error("プロジェクトにmodがありません"));
    }
    let selected_ids: Option<HashSet<_>> =
        selected_mod_unit_ids.map(|ids| ids.iter().copied().collect());
    if let (Some(selected), Some(requested)) = (&selected_ids, selected_mod_unit_ids) {
        let known: HashSet<_> = units.iter().map(|unit| unit.id).collect();
        if selected.len() != requested.len() || selected.iter().any(|id| !known.contains(id)) {
            return Err(invalid_selected_mods_error());
        }
    }
    if kind == ExportKindInput::ModpackFiles {
        let selected_units: Vec<_> = units
            .into_iter()
            .filter(|unit| {
                selected_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&unit.id))
                    && unit.source_kind == ModUnitSourceKind::Modpack
            })
            .collect();
        return validate_modpack_files_for_units(opened, &selected_units);
    }
    let mut combined = ExportValidationResult {
        issues: Vec::new(),
        counts: crate::commands::project::ProjectCounts {
            total: 0,
            translated: 0,
            missing: 0,
            needs_review: 0,
        },
        is_signed: false,
        minecraft_version_detection: None,
    };
    let mut matched_selection = false;
    let selected_shaderpack_only = selected_ids.as_ref().is_some_and(|ids| {
        !ids.is_empty()
            && units
                .iter()
                .filter(|unit| ids.contains(&unit.id))
                .all(|unit| unit.source_kind == ModUnitSourceKind::Shaderpack)
    });
    for unit in units.into_iter().filter(|unit| {
        selected_ids
            .as_ref()
            .is_none_or(|ids| ids.contains(&unit.id))
            && match kind {
                ExportKindInput::ShaderpackZip => unit.source_kind == ModUnitSourceKind::Shaderpack,
                ExportKindInput::ModifiedJar | ExportKindInput::ResourcePack => {
                    unit.source_kind == ModUnitSourceKind::Jar
                        || (kind == ExportKindInput::ResourcePack
                            && unit.source_kind == ModUnitSourceKind::Modpack)
                }
                ExportKindInput::LanguageFile => true,
                ExportKindInput::ModpackFiles => false,
            }
    }) {
        let context = match export_context(opened, unit.id) {
            Ok(context) => context,
            Err(error) => {
                matched_selection = true;
                combined.issues.push(ExportIssue {
                    severity: "error".to_owned(),
                    code: "modUnavailable".to_owned(),
                    message: error.user_message,
                    mod_unit_id: Some(unit.id),
                    mod_name: Some(export_unit_label(&unit)),
                    count: None,
                    namespace: None,
                    key: None,
                    display_label: None,
                    source_text: None,
                    target_text: None,
                });
                continue;
            }
        };
        let has_selection = match kind {
            ExportKindInput::LanguageFile => context.namespaces.iter().any(|candidate| {
                candidate.included
                    && Some(candidate.namespace.as_str()) == namespace
                    // FTB Quests uses document SNBT, which is not a language-file export.
                    && context
                        .language_files
                        .iter()
                        .any(|language| language.namespace == candidate.namespace)
            }),
            ExportKindInput::ShaderpackZip => context
                .namespaces
                .iter()
                .any(|candidate| candidate.included),
            ExportKindInput::ResourcePack | ExportKindInput::ModifiedJar => context
                .namespaces
                .iter()
                .any(|candidate| candidate.included),
            ExportKindInput::ModpackFiles => unreachable!("Modpack validation is handled above"),
        };
        if !has_selection {
            continue;
        }
        matched_selection = true;
        let validation_namespace = if kind == ExportKindInput::ShaderpackZip {
            context
                .namespaces
                .iter()
                .find(|candidate| candidate.included)
                .map(|candidate| candidate.namespace.as_str())
        } else {
            namespace
        };
        let validation_kind = if kind == ExportKindInput::ShaderpackZip {
            ExportKindInput::LanguageFile
        } else {
            kind
        };
        let result = validate_export_context(
            &context,
            validation_kind,
            minecraft_version,
            validation_namespace,
            manual_format,
        )?;
        combined.issues.extend(result.issues);
        combined.counts.total += result.counts.total;
        combined.counts.translated += result.counts.translated;
        combined.counts.missing += result.counts.missing;
        combined.counts.needs_review += result.counts.needs_review;
        combined.is_signed |= result.is_signed;
        if combined.minecraft_version_detection.is_none() {
            combined.minecraft_version_detection = result.minecraft_version_detection;
        }
    }
    if !matched_selection {
        combined.issues.push(ExportIssue {
            severity: "error".to_owned(),
            code: "invalidExportSelection".to_owned(),
            message: if selected_shaderpack_only
                && matches!(kind, ExportKindInput::ResourcePack | ExportKindInput::ModifiedJar)
            {
                "シェーダーパックはリソースパックや Mod JAR には含められません。シェーダーパック形式で書き出してください。".to_owned()
            } else if selected_mod_unit_ids.is_some() {
                "書き出すmodを選んでください。".to_owned()
            } else if kind == ExportKindInput::LanguageFile {
                "書き出すNamespaceを選び直してください。".to_owned()
            } else {
                "翻訳対象のNamespaceを選んでください。".to_owned()
            },
            mod_unit_id: None,
            mod_name: None,
            count: None,
            namespace: None,
            key: None,
            display_label: None,
            source_text: None,
            target_text: None,
        });
    }
    Ok(combined)
}

fn invalid_selected_mods_error() -> AppError {
    AppError::simple(
        "invalid_export_selection",
        "書き出すmodを選び直してください。",
    )
}

fn selected_units(
    opened: &OpenedProject,
    selected_ids: &[i64],
) -> Result<Vec<ModUnitRecord>, AppError> {
    if selected_ids.is_empty() {
        return Err(invalid_selected_mods_error());
    }
    let selected: HashSet<_> = selected_ids.iter().copied().collect();
    if selected.len() != selected_ids.len() {
        return Err(invalid_selected_mods_error());
    }
    let units = opened
        .repository()
        .list_mod_units()
        .map_err(|error| project_error(error.to_string()))?;
    let known: HashSet<_> = units.iter().map(|unit| unit.id).collect();
    if selected.iter().any(|id| !known.contains(id)) {
        return Err(invalid_selected_mods_error());
    }
    Ok(units
        .into_iter()
        .filter(|unit| selected.contains(&unit.id))
        .collect())
}

fn export_selected_merged_resource_pack_core(
    opened: &OpenedProject,
    request: MergedPackExportRequest<'_>,
    exported_paths: &Arc<std::sync::Mutex<HashSet<PathBuf>>>,
) -> Result<MergedPackExportOutcome, AppError> {
    let MergedPackExportRequest {
        selected_ids,
        requested_output,
        minecraft_version,
        manual_format,
        direct_install,
        overwrite,
    } = request;
    let units = selected_units(opened, selected_ids)?;
    let mut contexts = Vec::with_capacity(units.len());
    let mut failed_mod_results = Vec::new();
    for unit in units {
        if unit.source_kind == ModUnitSourceKind::Shaderpack {
            failed_mod_results.push(mod_export_result(
                unit.id,
                export_unit_label(&unit),
                "skipped",
                None,
                "シェーダーパックはリソースパックに含めません。「日本語化したシェーダーパック」から書き出してください。"
                    .to_owned(),
            ));
            continue;
        }
        match export_context(opened, unit.id) {
            Ok(context) => contexts.push((unit, context)),
            Err(error) => failed_mod_results.push(mod_export_result(
                unit.id,
                export_unit_label(&unit),
                "failed",
                None,
                error.user_message,
            )),
        }
    }
    let contributing: Vec<_> = contexts
        .iter()
        .filter(|(_, context)| {
            context
                .namespaces
                .iter()
                .any(|namespace| namespace.included)
        })
        .collect();
    let labels: Vec<_> = contributing
        .iter()
        .map(|(unit, _)| export_unit_label(unit))
        .collect();
    let merged_mods: Vec<_> = contributing
        .iter()
        .zip(&labels)
        .map(|((unit, context), label)| MergedPackMod {
            input: context.input(),
            info: ResourcePackMod {
                name: unit.name.clone(),
                version: unit.version.clone(),
            },
            label,
        })
        .collect();
    if merged_mods.is_empty() {
        if failed_mod_results.is_empty() {
            return Err(invalid_export_selection_error());
        }
        return Err(AppError::simple(
            "export_mods_unavailable",
            "選択したmodを読み込めませんでした。保存済みJARを確認してください。",
        ));
    }

    let summary = opened.summary();
    let project_pack_name = summary
        .instance_path
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| summary.display_name.clone());
    let output = if direct_install {
        let instance_path = summary.instance_path.as_deref().ok_or_else(|| {
            AppError::simple(
                "invalid_output_path",
                "インスタンスの保存先がありません。通常の保存先を選んでください。",
            )
        })?;
        PathBuf::from(instance_path)
            .join("resourcepacks")
            .join(merged_resource_pack_filename(
                &merged_mods,
                &project_pack_name,
            ))
    } else {
        requested_output
            .ok_or_else(|| AppError::simple("invalid_output_path", "保存先を選んでください。"))?
            .to_path_buf()
    };
    ensure_file_extension(&output, "zip")?;
    if direct_install {
        let parent = output.parent().ok_or_else(|| {
            AppError::simple("invalid_output_path", "保存先フォルダを確認してください。")
        })?;
        fs::create_dir_all(parent).map_err(export_error)?;
    }
    for (_, context) in &contexts {
        ensure_output_is_safe(&context.source_jar, &output)?;
    }
    if direct_install && output.exists() && !overwrite {
        return Ok(MergedPackExportOutcome::OverwriteRequired {
            output_path: output.to_string_lossy().into_owned(),
        });
    }

    let staging = opened
        .directory()
        .join(format!(".yakucraft-export-{}.zip", uuid::Uuid::new_v4()));
    let pack_name = if merged_mods.len() == 1 {
        merged_mods[0].label
    } else {
        merged_resource_pack_display_name(&project_pack_name)
    };
    let pack_description = format!("{pack_name} の日本語訳（{} mod）", merged_mods.len());
    let result = (|| {
        let report = export_merged_resource_pack(
            &merged_mods,
            MergedResourcePackOptions {
                minecraft_version: Some(minecraft_version),
                manual_format: manual_format.map(|format| ManualPackFormat {
                    major: format.major,
                    minor: format.minor,
                }),
                pack_description: Some(&pack_description),
            },
            &staging,
            false,
        )
        .map_err(export_operation_error)?;
        let output = if direct_install {
            // stagingのUUID名ではなく、上書き確認に使った予定パスへatomicに配置する。
            install_resource_pack_to(&staging, &output, overwrite)
                .map_err(export_operation_error)?
        } else {
            let output = requested_output.expect("normal export requires an output path");
            copy_atomic_selected(&staging, output).map_err(export_error)?;
            output.to_path_buf()
        };
        record_exported_path(exported_paths, &output)?;
        let mut mod_results: Vec<_> = contexts
            .iter()
            .map(|(unit, context)| {
                let included = context
                    .namespaces
                    .iter()
                    .any(|namespace| namespace.included);
                mod_export_result(
                    unit.id,
                    export_unit_label(unit),
                    if included { "success" } else { "skipped" },
                    included.then(|| output.to_string_lossy().into_owned()),
                    if included {
                        "リソースパックに含めました。".to_owned()
                    } else {
                        "翻訳対象のNamespaceがありません。".to_owned()
                    },
                )
            })
            .collect();
        mod_results.extend(failed_mod_results);
        Ok(MergedPackExportOutcome::Completed {
            output_path: output.to_string_lossy().into_owned(),
            directly_installed: direct_install,
            conflicts: report.conflicts,
            mod_results,
        })
    })();
    let _ = fs::remove_file(staging);
    result
}

#[cfg(test)]
pub(crate) fn benchmark_export_selected_merged_resource_pack(
    opened: &OpenedProject,
    selected_ids: &[i64],
    minecraft_version: &str,
) -> Result<MergedPackExportOutcome, AppError> {
    export_selected_merged_resource_pack_core(
        opened,
        MergedPackExportRequest {
            selected_ids,
            requested_output: None,
            minecraft_version,
            manual_format: None,
            direct_install: true,
            overwrite: true,
        },
        &Arc::new(std::sync::Mutex::new(HashSet::new())),
    )
}

fn export_selected_modified_jars_core(
    opened: &OpenedProject,
    selected_ids: &[i64],
    output_directory: &Path,
    replace_in_place: bool,
    exported_paths: &Arc<std::sync::Mutex<HashSet<PathBuf>>>,
) -> Result<Vec<ModExportResult>, AppError> {
    let units: Vec<_> = selected_units(opened, selected_ids)?
        .into_iter()
        .filter(|unit| unit.source_kind == ModUnitSourceKind::Jar)
        .collect();
    if units.is_empty() {
        return Err(AppError::simple(
            "jar_unit_required",
            "Mod JAR を選んでください。",
        ));
    }
    if replace_in_place && opened.summary().instance_path.is_none() {
        return Err(AppError::simple(
            "invalid_output_path",
            "このプロジェクトにはインスタンスの保存先がありません。",
        ));
    }
    if !replace_in_place {
        fs::create_dir_all(output_directory).map_err(export_error)?;
    }
    let repository = opened.repository();
    let mut results = Vec::with_capacity(units.len());
    for unit in units {
        let label = export_unit_label(&unit);
        if replace_in_place
            && Path::new(&unit.original_path).is_file()
            && opened
                .summary()
                .instance_path
                .as_deref()
                .is_some_and(|instance_path| {
                    !original_jar_is_in_instance_mods(
                        Path::new(instance_path),
                        Path::new(&unit.original_path),
                    )
                })
        {
            results.push(mod_export_result(
                unit.id,
                label,
                "skipped",
                None,
                "インスタンスの mods フォルダ外のため直接置き換えできません。".to_owned(),
            ));
            continue;
        }
        let context = match export_context(opened, unit.id) {
            Ok(context) => context,
            Err(error) => {
                results.push(mod_export_result(
                    unit.id,
                    label,
                    "failed",
                    None,
                    error.user_message,
                ));
                continue;
            }
        };
        if !context
            .namespaces
            .iter()
            .any(|namespace| namespace.included)
        {
            results.push(mod_export_result(
                unit.id,
                label,
                "skipped",
                None,
                "翻訳対象のNamespaceがありません。".to_owned(),
            ));
            continue;
        }

        if replace_in_place {
            let target = PathBuf::from(&unit.original_path);
            if !target.is_file() {
                results.push(mod_export_result(
                    unit.id,
                    label,
                    "skipped",
                    None,
                    "元のJARが見つかりません。".to_owned(),
                ));
                continue;
            }
            let active = match repository.active_jar_replacement(unit.id, &unit.original_path) {
                Ok(active) => active,
                Err(error) => {
                    results.push(mod_export_result(
                        unit.id,
                        label,
                        "failed",
                        None,
                        error.to_string(),
                    ));
                    continue;
                }
            };
            if let Some(record) = &active
                && let Err(error) = validate_project_backup_path(
                    opened.directory(),
                    Path::new(&record.backup_path),
                    &record.previous_sha256,
                )
            {
                results.push(mod_export_result(
                    unit.id,
                    label,
                    "failed",
                    None,
                    error.user_message,
                ));
                continue;
            }
            let current_hash = match sha256_file_hex(&target) {
                Ok(hash) => hash,
                Err(error) => {
                    results.push(mod_export_result(
                        unit.id,
                        label,
                        "failed",
                        None,
                        error.user_message,
                    ));
                    continue;
                }
            };
            match jar_overwrite_decision(
                &current_hash,
                &unit.source_sha256,
                active.as_ref().map(|record| record.written_sha256.as_str()),
            ) {
                JarOverwriteDecision::Changed => {
                    results.push(mod_export_result(
                        unit.id,
                        label,
                        "skipped",
                        None,
                        "元のJARが更新されています。現在のファイルは変更しませんでした。"
                            .to_owned(),
                    ));
                    continue;
                }
                JarOverwriteDecision::Original | JarOverwriteDecision::PreviousWrite => {}
            }

            let backup_directory = opened.directory().join("backups");
            // 書き出しはプロジェクト内の不変コピーを元にする。再上書きでも初回のバックアップを維持し、
            // 復元時にYakuCraft適用前のインスタンスJARへ戻せるようにする。
            let replacement = match &active {
                Some(record) => replace_jar_in_place_with_backup(
                    &context.input(),
                    &context.source_jar,
                    &target,
                    Path::new(&record.backup_path),
                ),
                None => replace_jar_in_place(
                    &context.input(),
                    &context.source_jar,
                    &target,
                    &backup_directory,
                ),
            };
            let replacement = match replacement {
                Ok(replacement) => replacement,
                Err(error) => {
                    results.push(mod_export_result(
                        unit.id,
                        label,
                        "failed",
                        None,
                        export_operation_error(error).user_message,
                    ));
                    continue;
                }
            };
            let record = JarReplacementRecord {
                id: active.as_ref().map_or(0, |record| record.id),
                mod_unit_id: unit.id,
                target_path: unit.original_path.clone(),
                backup_path: active.as_ref().map_or_else(
                    || replacement.backup_path.to_string_lossy().into_owned(),
                    |record| record.backup_path.clone(),
                ),
                previous_sha256: active.as_ref().map_or_else(
                    || replacement.previous_sha256.clone(),
                    |record| record.previous_sha256.clone(),
                ),
                written_sha256: replacement.written_sha256,
                created_at: export_timestamp(),
                restored_at: None,
            };
            let backup_path = record.backup_path.clone();
            if let Err(error) = opened.writer().save_jar_replacement(record) {
                let restore = restore_jar_from_backup(Path::new(&backup_path), &target);
                let message = match restore {
                    Ok(()) => {
                        "書き出し記録を保存できなかったため、元のJARへ戻しました。".to_owned()
                    }
                    Err(restore_error) => format!(
                        "書き出し記録を保存できませんでした。元のJARへの復元にも失敗しました: {}",
                        export_operation_error(restore_error).user_message
                    ),
                };
                tracing::error!(mod_unit_id = unit.id, error = %error, "failed to persist in-place JAR replacement");
                results.push(mod_export_result(unit.id, label, "failed", None, message));
                continue;
            }
            if let Err(error) = record_exported_path(exported_paths, &target) {
                results.push(mod_export_result(
                    unit.id,
                    label,
                    "failed",
                    Some(target.to_string_lossy().into_owned()),
                    error.user_message,
                ));
                continue;
            }
            results.push(mod_export_result(
                unit.id,
                label,
                "success",
                Some(target.to_string_lossy().into_owned()),
                "JARを上書きしました。元のJARはプロジェクト内に保存されています。".to_owned(),
            ));
        } else {
            let file_stem = Path::new(&unit.source_filename)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("mod");
            let filename = format!("{}-{}-ja.jar", safe_file_component(file_stem), unit.id);
            let output = output_directory.join(filename);
            if output.exists() {
                results.push(mod_export_result(
                    unit.id,
                    label,
                    "skipped",
                    None,
                    "同名のファイルが既にあります。既存ファイルは変更していません。".to_owned(),
                ));
                continue;
            }
            let result = ensure_output_is_safe(&context.source_jar, &output)
                .and_then(|()| ensure_file_extension(&output, "jar"))
                .and_then(|()| {
                    // overwrite=false also closes the race between the existence check and publish.
                    write_modified_jar(&context.input(), &context.source_jar, &output, false)
                        .map_err(export_operation_error)?;
                    record_exported_path(exported_paths, &output)?;
                    Ok(())
                });
            match result {
                Ok(()) => results.push(mod_export_result(
                    unit.id,
                    label,
                    "success",
                    Some(output.to_string_lossy().into_owned()),
                    "JARを書き出しました。".to_owned(),
                )),
                Err(error) => results.push(mod_export_result(
                    unit.id,
                    label,
                    "failed",
                    None,
                    error.user_message,
                )),
            }
        }
    }
    Ok(results)
}

#[cfg(test)]
pub(crate) fn benchmark_export_selected_modified_jars(
    opened: &OpenedProject,
    selected_ids: &[i64],
    output_directory: &Path,
    replace_in_place: bool,
) -> Result<Vec<ModExportResult>, AppError> {
    export_selected_modified_jars_core(
        opened,
        selected_ids,
        output_directory,
        replace_in_place,
        &Arc::new(std::sync::Mutex::new(HashSet::new())),
    )
}

fn export_selected_language_files_core(
    opened: &OpenedProject,
    selected_ids: &[i64],
    output_directory: &Path,
    namespace: &str,
    exported_paths: &Arc<std::sync::Mutex<HashSet<PathBuf>>>,
) -> Result<Vec<ModExportResult>, AppError> {
    let units = selected_units(opened, selected_ids)?;
    fs::create_dir_all(output_directory).map_err(export_error)?;
    let mut results = Vec::with_capacity(units.len());
    for unit in units {
        let label = export_unit_label(&unit);
        let context = match export_context(opened, unit.id) {
            Ok(context) => context,
            Err(error) => {
                results.push(mod_export_result(
                    unit.id,
                    label,
                    "failed",
                    None,
                    error.user_message,
                ));
                continue;
            }
        };
        let namespaces: Vec<_> = context
            .namespaces
            .iter()
            .map(|candidate| NamespaceExport {
                namespace: candidate.namespace.clone(),
                included: candidate.included && candidate.namespace == namespace,
            })
            .collect();
        if !namespaces.iter().any(|candidate| candidate.included) {
            results.push(mod_export_result(
                unit.id,
                label,
                "skipped",
                None,
                "このmodに選択したNamespaceはありません。".to_owned(),
            ));
            continue;
        }
        if unit.source_kind == ModUnitSourceKind::Shaderpack {
            let (file_name, bytes) = match shaderpack_lang_export(&context, namespace) {
                Ok(export) => export,
                Err(error) => {
                    results.push(mod_export_result(
                        unit.id,
                        label,
                        "failed",
                        None,
                        error.user_message,
                    ));
                    continue;
                }
            };
            let stem = Path::new(&unit.source_filename)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("shaderpack");
            let unit_directory =
                output_directory.join(format!("{}-{}", safe_file_component(stem), unit.id));
            let output = unit_directory.join(file_name);
            if output.exists() {
                results.push(mod_export_result(
                    unit.id,
                    label,
                    "skipped",
                    None,
                    "同名のファイルが既にあります。既存ファイルは変更していません。".to_owned(),
                ));
                continue;
            }
            let result = (|| {
                fs::create_dir_all(&unit_directory).map_err(export_error)?;
                ensure_output_is_safe(&context.source_jar, &output)?;
                ensure_output_is_within_directory(output_directory, &output)?;
                atomic_write_selected_if_absent(&output, &bytes).map_err(export_error)?;
                record_exported_path(exported_paths, &output)
            })();
            match result {
                Ok(()) => results.push(mod_export_result(
                    unit.id,
                    label,
                    "success",
                    Some(output.to_string_lossy().into_owned()),
                    "言語ファイルを書き出しました。".to_owned(),
                )),
                Err(error) => results.push(mod_export_result(
                    unit.id,
                    label,
                    "failed",
                    None,
                    error.user_message,
                )),
            }
            continue;
        }
        let input = ExportInput {
            namespaces: &namespaces,
            ..context.input()
        };
        let artifact = match yc_export::create_language_files(&input) {
            Ok(mut artifacts) => artifacts.pop(),
            Err(error) => {
                results.push(mod_export_result(
                    unit.id,
                    label,
                    "failed",
                    None,
                    export_operation_error(error).user_message,
                ));
                continue;
            }
        };
        let document_artifacts = match yc_export::create_patchouli_document_files(&input) {
            Ok(documents) => documents,
            Err(error) => {
                results.push(mod_export_result(
                    unit.id,
                    label,
                    "failed",
                    None,
                    export_operation_error(error).user_message,
                ));
                continue;
            }
        };
        if artifact.is_none() && document_artifacts.is_empty() {
            results.push(mod_export_result(
                unit.id,
                label,
                "skipped",
                None,
                "書き出す翻訳がありません。".to_owned(),
            ));
            continue;
        }
        let stem = Path::new(&unit.source_filename)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("mod");
        let unit_directory =
            output_directory.join(format!("{}-{}", safe_file_component(stem), unit.id));
        let output = artifact
            .as_ref()
            .map(|artifact| unit_directory.join(&artifact.file_name));
        let document_outputs: Vec<_> = document_artifacts
            .iter()
            .map(|document| unit_directory.join(&document.path))
            .collect();
        if output.as_ref().is_some_and(|output| output.exists())
            || document_outputs.iter().any(|output| output.exists())
        {
            results.push(mod_export_result(
                unit.id,
                label,
                "skipped",
                None,
                "同名のファイルが既にあります。既存ファイルは変更していません。".to_owned(),
            ));
            continue;
        }
        let result = (|| {
            // canonicalizeには親フォルダが必要なため、先に選択先を検証してModフォルダを作成し、
            // 作成後にシンボリックリンクも解決した実際の出力パスをもう一度検証する。
            fs::canonicalize(output_directory).map_err(export_error)?;
            fs::create_dir_all(&unit_directory).map_err(export_error)?;
            if let (Some(output), Some(artifact)) = (output.as_ref(), artifact.as_ref()) {
                ensure_output_is_safe(&context.source_jar, output)?;
                ensure_output_is_within_directory(output_directory, output)?;
                atomic_write_selected_if_absent(output, &artifact.bytes).map_err(export_error)?;
                record_exported_path(exported_paths, output)?;
            }
            for document in &document_artifacts {
                let document_output = unit_directory.join(&document.path);
                ensure_output_is_safe(&context.source_jar, &document_output)?;
                if let Some(parent) = document_output.parent() {
                    fs::create_dir_all(parent).map_err(export_error)?;
                }
                ensure_output_is_within_directory(output_directory, &document_output)?;
            }
            let written = yc_export::write_patchouli_document_files(
                &unit_directory,
                &document_artifacts,
                false,
            )
            .map_err(export_error)?;
            for path in &written {
                record_exported_path(exported_paths, path)?;
            }
            Ok::<(), AppError>(())
        })();
        match result {
            Ok(()) => {
                let result_path = output.or_else(|| {
                    document_artifacts
                        .first()
                        .map(|document| unit_directory.join(&document.path))
                });
                results.push(mod_export_result(
                    unit.id,
                    label,
                    "success",
                    result_path.map(|path| path.to_string_lossy().into_owned()),
                    "翻訳データを書き出しました。".to_owned(),
                ))
            }
            Err(error) => results.push(mod_export_result(
                unit.id,
                label,
                "failed",
                None,
                error.user_message,
            )),
        }
    }
    Ok(results)
}

pub(crate) fn export_modpack_files_core(
    opened: &OpenedProject,
    selected_ids: &[i64],
    output_path: Option<&Path>,
    direct_install: bool,
    overwrite: bool,
    exported_paths: &Arc<std::sync::Mutex<HashSet<PathBuf>>>,
) -> Result<ModpackFilesExportOutcome, AppError> {
    let selected = selected_units(opened, selected_ids)?;
    let units: Vec<_> = selected
        .into_iter()
        .filter(|unit| unit.source_kind == ModUnitSourceKind::Modpack)
        .collect();
    if units.is_empty() {
        return Err(AppError::simple(
            "modpack_unit_required",
            "Modpack 固有の項目を選んでください。",
        ));
    }
    let repository = opened.repository();
    let mut files = BTreeMap::<String, Vec<u8>>::new();
    let mut direct_files = Vec::new();
    for unit in &units {
        let source = repository
            .mod_unit_source_path(unit.id)
            .map_err(|error| project_error(error.to_string()))?;
        let (analysis, data) = super::modpack::analyze_modpack_archive(&source)?;
        let settings = repository
            .review_settings_for_unit(unit.id)
            .map_err(|error| project_error(error.to_string()))?;
        let (translations, excluded_kubejs_output_paths) =
            modpack_export_translations(opened, unit.id, &data, &settings)?;
        let output_files = yc_modpack::write_ja_jp(&data, &translations)
            .map_err(|error| export_error(error.safe_detail()))?
            .into_iter()
            .filter(|output| !excluded_kubejs_output_paths.contains(&output.path))
            .collect::<Vec<_>>();
        let instance_root = modpack_instance_root(unit, &data.scan.game_directory)?;
        for output in &output_files {
            if direct_install {
                let relative = safe_modpack_relative_path(&output.path)?;
                direct_files.push((
                    unit,
                    instance_root.clone(),
                    instance_root.join(relative),
                    output.bytes.clone(),
                ));
            }
            if let Some(previous) = files.get(&output.path)
                && previous != &output.bytes
            {
                return Err(AppError::new(
                    "modpack_output_conflict",
                    "選択した Modpack の出力先が重複しています。",
                    Some(output.path.clone()),
                ));
            }
            files.insert(output.path.clone(), output.bytes.clone());
        }
        let _ = analysis;
    }

    if direct_install {
        let mut seen = HashSet::new();
        for (_, _, target, _) in &direct_files {
            if !seen.insert(target.clone()) {
                return Err(AppError::new(
                    "modpack_output_conflict",
                    "選択した Modpack の配置先が重複しています。",
                    Some(target.to_string_lossy().into_owned()),
                ));
            }
        }
        for (unit, instance_root, target, _) in &direct_files {
            validate_modpack_target_path(instance_root, target, false)?;
            let target_string = target.to_string_lossy();
            let active = repository
                .active_file_deployment(unit.id, &target_string)
                .map_err(|error| project_error(error.to_string()))?;
            if let Some(record) = active.as_ref() {
                ensure_modpack_deployment_unchanged(target, &record.deployed_sha256)?;
            } else if target.exists() && !overwrite {
                return Ok(ModpackFilesExportOutcome::OverwriteRequired {
                    target_path: target.to_string_lossy().into_owned(),
                });
            }
        }
        let mut first_target = None;
        for (unit, instance_root, target, bytes) in direct_files {
            deploy_modpack_file(
                opened,
                unit,
                &instance_root,
                &target,
                &bytes,
                exported_paths,
            )?;
            first_target.get_or_insert(target);
        }
        return Ok(ModpackFilesExportOutcome::Completed {
            output_path: first_target
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            directly_installed: true,
            file_count: files.len(),
        });
    }

    let output_path = output_path.ok_or_else(|| {
        AppError::simple(
            "invalid_output_path",
            "Modpack ZIP の保存先を選んでください。",
        )
    })?;
    ensure_file_extension(output_path, "zip")?;
    let zip_bytes = write_modpack_output_zip(&files)?;
    if output_path.exists() && !overwrite {
        return Ok(ModpackFilesExportOutcome::OverwriteRequired {
            target_path: output_path.to_string_lossy().into_owned(),
        });
    }
    let parent = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(export_error)?;
    atomic_write_selected(output_path, &zip_bytes).map_err(export_error)?;
    record_exported_path(exported_paths, output_path)?;
    Ok(ModpackFilesExportOutcome::Completed {
        output_path: output_path.to_string_lossy().into_owned(),
        directly_installed: false,
        file_count: files.len(),
    })
}

// 複数パックの `option.*` キーは衝突するため、Resource Pack や JAR に混ぜず各パック内へ書き出す。
pub(crate) fn export_selected_shaderpacks_core(
    opened: &OpenedProject,
    selected_ids: &[i64],
    output_directory: Option<&Path>,
    direct_install: bool,
    overwrite: bool,
    exported_paths: &Arc<std::sync::Mutex<HashSet<PathBuf>>>,
) -> Result<ShaderpackExportOutcome, AppError> {
    let selected = selected_units(opened, selected_ids)?;
    let units: Vec<_> = selected
        .into_iter()
        .filter(|unit| unit.source_kind == ModUnitSourceKind::Shaderpack)
        .collect();
    if units.is_empty() {
        return Err(AppError::simple(
            "shaderpack_unit_required",
            "シェーダーパックを選んでください。",
        ));
    }
    let instance_root = if direct_install {
        Some(shaderpack_instance_root(opened)?)
    } else {
        None
    };
    let output_directory = if direct_install {
        None
    } else {
        Some(output_directory.ok_or_else(|| {
            AppError::simple("invalid_output_path", "保存先フォルダを選んでください。")
        })?)
    };
    if let Some(output_directory) = output_directory {
        fs::create_dir_all(output_directory).map_err(export_error)?;
        fs::canonicalize(output_directory).map_err(export_error)?;
    }

    let original_paths: HashSet<_> = opened
        .repository()
        .list_mod_units()
        .map_err(|error| project_error(error.to_string()))?
        .iter()
        .filter_map(|unit| normalized_shaderpack_path(Path::new(&unit.original_path)).ok())
        .collect();

    let mut prepared = Vec::with_capacity(units.len());
    let mut used_names = HashSet::new();
    for unit in &units {
        let context = export_context(opened, unit.id)?;
        let namespace = context
            .namespaces
            .iter()
            .find(|candidate| candidate.included)
            .map(|candidate| candidate.namespace.as_str())
            .ok_or_else(|| {
                AppError::simple("invalid_export_selection", "翻訳対象を選んでください。")
            })?;
        let source_lang = context
            .language_files
            .iter()
            .find(|language| {
                language.namespace == namespace
                    && normalize_locale(&language.locale)
                        == normalize_locale(&context.source_locale)
            })
            .ok_or_else(|| {
                AppError::simple(
                    "shaderpack_source_language_missing",
                    "翻訳元の言語ファイルが見つかりません。",
                )
            })?;
        let translations = context
            .entries
            .iter()
            .filter(|entry| {
                entry.namespace == namespace && !is_empty_translation(&entry.target_text)
            })
            .map(|entry| (entry.key.clone(), entry.target_text.clone()))
            .collect::<Vec<_>>();
        let source_path = opened
            .repository()
            .mod_unit_source_path(unit.id)
            .map_err(|error| project_error(error.to_string()))?;
        let staging = opened
            .directory()
            .join(format!(".shaderpack-export-{}.zip", uuid::Uuid::new_v4()));
        let write_result = yc_shaderpack::write_translated_zip(
            &source_path,
            &staging,
            &source_lang.file_name,
            &translations,
        );
        if let Err(error) = write_result {
            let _ = fs::remove_file(&staging);
            return Err(AppError::new(
                "shaderpack_export",
                "シェーダーパックを書き出せませんでした。",
                Some(error.to_string()),
            ));
        }
        let bytes = fs::read(&staging).map_err(export_error);
        let remove_result = fs::remove_file(&staging).map_err(export_error);
        let bytes = bytes?;
        remove_result?;
        let base_name = shaderpack_output_filename(unit);
        let filename = if units
            .iter()
            .filter(|candidate| {
                shaderpack_output_filename(candidate).to_lowercase() == base_name.to_lowercase()
            })
            .count()
            > 1
        {
            let stem = Path::new(&base_name)
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("shaderpack-ja");
            format!("{stem}-{}.zip", unit.id)
        } else {
            base_name
        };
        let stem = Path::new(&filename)
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("shaderpack-ja")
            .to_owned();
        let directory = if let Some(instance_root) = instance_root.as_ref() {
            instance_root.join("shaderpacks")
        } else {
            output_directory
                .expect("selected output directory exists for folder exports")
                .to_path_buf()
        };
        let mut filename = filename;
        let mut suffix = 2;
        let target = loop {
            let mut target = directory.join(&filename);
            if directory.is_dir() {
                for entry in fs::read_dir(&directory).map_err(export_error)? {
                    let entry = entry.map_err(export_error)?;
                    if entry.file_name().to_string_lossy().to_lowercase() == filename.to_lowercase()
                    {
                        target = entry.path();
                        break;
                    }
                }
            }
            if !used_names.contains(&filename.to_lowercase())
                && !original_paths.contains(&normalized_shaderpack_path(&target)?)
            {
                used_names.insert(filename.to_lowercase());
                break target;
            }
            filename = format!("{stem}-{suffix}.zip");
            suffix += 1;
        };
        prepared.push((unit.clone(), source_path, target, bytes));
    }

    if direct_install {
        let instance_root = instance_root
            .as_ref()
            .expect("direct installation has an instance root");
        let mut seen = HashSet::new();
        let mut overwrite_required_paths = Vec::new();
        for (unit, _, target, _) in &prepared {
            validate_modpack_target_path(instance_root, target, true)?;
            if !seen.insert(target.clone()) {
                return Err(AppError::simple(
                    "shaderpack_output_conflict",
                    "選択したシェーダーパックの配置先が重複しています。",
                ));
            }
            let target_string = target.to_string_lossy();
            let active = opened
                .repository()
                .active_file_deployment(unit.id, &target_string)
                .map_err(|error| project_error(error.to_string()))?;
            if let Some(record) = active.as_ref() {
                ensure_modpack_deployment_unchanged(target, &record.deployed_sha256)?;
            } else if target.exists() && !overwrite {
                overwrite_required_paths.push(target.to_string_lossy().into_owned());
            }
        }
        if !overwrite_required_paths.is_empty() {
            return Ok(ShaderpackExportOutcome::OverwriteRequired {
                target_paths: overwrite_required_paths,
            });
        }
    } else {
        let mut overwrite_required_paths = Vec::new();
        for (_, source, target, _) in &prepared {
            ensure_output_is_safe(source, target)?;
            ensure_output_is_within_directory(
                output_directory.expect("selected output directory exists for folder exports"),
                target,
            )?;
            if target.exists() && !overwrite {
                overwrite_required_paths.push(target.to_string_lossy().into_owned());
            }
        }
        if !overwrite_required_paths.is_empty() {
            return Ok(ShaderpackExportOutcome::OverwriteRequired {
                target_paths: overwrite_required_paths,
            });
        }
    }

    let mut output_paths = Vec::new();
    let mut mod_results = Vec::with_capacity(prepared.len());
    for (unit, _, target, bytes) in prepared {
        let label = export_unit_label(&unit);
        let result = if direct_install {
            deploy_modpack_file(
                opened,
                &unit,
                instance_root.as_deref().expect("instance root was checked"),
                &target,
                &bytes,
                exported_paths,
            )
        } else {
            atomic_write_selected(&target, &bytes)
                .map_err(export_error)
                .and_then(|()| record_exported_path(exported_paths, &target))
        };
        match result {
            Ok(()) => {
                let output_path = target.to_string_lossy().into_owned();
                output_paths.push(output_path.clone());
                mod_results.push(mod_export_result(
                    unit.id,
                    label,
                    "success",
                    Some(output_path),
                    "シェーダーパックを書き出しました。".to_owned(),
                ));
            }
            Err(error) => mod_results.push(mod_export_result(
                unit.id,
                label,
                "failed",
                None,
                error.user_message,
            )),
        }
    }
    Ok(ShaderpackExportOutcome::Completed {
        output_paths,
        directly_installed: direct_install,
        mod_results,
    })
}

fn shaderpack_output_filename(unit: &ModUnitRecord) -> String {
    let stem = Path::new(&unit.source_filename)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(&unit.source_filename);
    format!("{}-ja.zip", safe_file_component(stem))
}

fn shaderpack_instance_root(opened: &OpenedProject) -> Result<PathBuf, AppError> {
    let instance_path =
        opened.summary().instance_path.as_deref().ok_or_else(|| {
            AppError::simple("instance_required", "インスタンスを検出できません。")
        })?;
    let root = yc_modpack::resolve_game_directory(Path::new(instance_path));
    if !root.is_dir() {
        return Err(AppError::simple(
            "instance_required",
            "インスタンスを検出できません。",
        ));
    }
    Ok(root)
}

fn shaderpack_lang_export(
    context: &ExportContext,
    namespace: &str,
) -> Result<(String, Vec<u8>), AppError> {
    let source = context
        .language_files
        .iter()
        .find(|language| {
            language.namespace == namespace
                && normalize_locale(&language.locale) == normalize_locale(&context.source_locale)
        })
        .ok_or_else(|| {
            AppError::simple(
                "shaderpack_source_language_missing",
                "翻訳元の言語ファイルが見つかりません。",
            )
        })?;
    let entries = context
        .entries
        .iter()
        .filter(|entry| entry.namespace == namespace && !is_empty_translation(&entry.target_text))
        .map(|entry| (entry.key.clone(), entry.target_text.clone()))
        .collect::<Vec<_>>();
    Ok((
        yc_shaderpack::japanese_filename(&source.file_name).to_owned(),
        yc_shaderpack::write_lang(&entries),
    ))
}

fn modpack_export_translations(
    opened: &OpenedProject,
    mod_unit_id: i64,
    data: &yc_modpack::ModpackData,
    settings: &ReviewSettings,
) -> Result<(BTreeMap<String, String>, HashSet<String>), AppError> {
    let repository = opened.repository();
    let saved_lang = repository
        .list_entries(Some(mod_unit_id))
        .map_err(|error| project_error(error.to_string()))?;
    let saved_documents = repository
        .list_document_entries(Some(mod_unit_id))
        .map_err(|error| project_error(error.to_string()))?;
    let excluded_kubejs_output_paths: HashSet<_> = data
        .scan
        .files
        .iter()
        .filter(|file| file.kind == yc_modpack::ModpackFileKind::KubeJsLanguage)
        .filter(|file| {
            file.namespace
                .as_ref()
                .is_some_and(|namespace| !settings.included_namespaces.contains(namespace))
        })
        .filter_map(|file| {
            let (directory, _) = file.path.rsplit_once('/')?;
            Some(format!("{directory}/ja_jp.json"))
        })
        .collect();
    let mut translations = BTreeMap::new();
    for entry in &data.entries {
        let saved_translation = if let Some(key) = entry.key.strip_prefix("ftbquests:") {
            saved_documents
                .iter()
                .find(|saved| {
                    saved.entry_kind == TranslationEntryKind::Ftbquests && saved.key == entry.key
                })
                .and_then(|saved| saved.target_text.as_ref())
                .or_else(|| {
                    saved_documents
                        .iter()
                        .find(|saved| {
                            saved.entry_kind == TranslationEntryKind::Ftbquests && saved.key == key
                        })
                        .and_then(|saved| saved.target_text.as_ref())
                })
        } else if let Some(rest) = entry.key.strip_prefix("kubejs:") {
            let (namespace, key) = rest.split_once('#').unwrap_or(("", rest));
            saved_lang
                .iter()
                .find(|saved| saved.namespace == namespace && saved.key == key)
                .and_then(|saved| saved.target_text.as_ref())
        } else {
            None
        };
        if let Some(target) = saved_translation {
            if is_empty_translation(target) {
                if entry.key.starts_with("kubejs:") {
                    // Explicitly cleared KubeJS translations suppress the previous Japanese value.
                    translations.insert(entry.key.clone(), target.clone());
                } else if entry.key.starts_with("ftbquests:")
                    && !settings.preserve_existing_japanese
                    && entry.translatable
                {
                    translations.insert(entry.key.clone(), entry.source_text.clone());
                }
            } else {
                translations.insert(entry.key.clone(), target.clone());
            }
        } else if !settings.preserve_existing_japanese && entry.translatable {
            if entry.key.starts_with("ftbquests:") {
                translations.insert(entry.key.clone(), entry.source_text.clone());
            } else if entry.key.starts_with("kubejs:") {
                // An empty value tells the KubeJS writer to omit this key and its old translation.
                translations.insert(entry.key.clone(), String::new());
            }
        }
    }
    Ok((translations, excluded_kubejs_output_paths))
}

fn validate_modpack_files_for_units(
    opened: &OpenedProject,
    units: &[ModUnitRecord],
) -> Result<ExportValidationResult, AppError> {
    let mut issues = Vec::new();
    for unit in units {
        let source = opened
            .repository()
            .mod_unit_source_path(unit.id)
            .map_err(|error| project_error(error.to_string()))?;
        let (_analysis, data) = super::modpack::analyze_modpack_archive(&source)?;
        let settings = opened
            .repository()
            .review_settings_for_unit(unit.id)
            .map_err(|error| project_error(error.to_string()))?;
        let (translations, _) = modpack_export_translations(opened, unit.id, &data, &settings)?;

        for entry in &data.entries {
            let (namespace, key, profile, display_label) = if entry.key.starts_with("ftbquests:") {
                (
                    "ftbquests",
                    format!("{}#{}", entry.document_path, entry.pointer),
                    PlaceholderProfile::FtbQuests,
                    Some(ftb_quests_warning_label(
                        &entry.document_path,
                        &entry.pointer,
                    )),
                )
            } else if let Some(rest) = entry.key.strip_prefix("kubejs:") {
                let (namespace, key) = rest.split_once('#').unwrap_or(("", rest));
                if !settings
                    .included_namespaces
                    .iter()
                    .any(|included| included == namespace)
                {
                    continue;
                }
                (namespace, key.to_owned(), PlaceholderProfile::Lang, None)
            } else {
                continue;
            };
            if entry.key.starts_with("kubejs:")
                && translations
                    .get(&entry.key)
                    .is_some_and(|target| is_empty_translation(target))
            {
                continue;
            }
            let target = if entry.translatable {
                translations
                    .get(&entry.key)
                    .filter(|target| !is_empty_translation(target))
                    .or_else(|| {
                        entry
                            .existing_translation
                            .as_ref()
                            .filter(|target| !is_empty_translation(target))
                    })
                    .unwrap_or(&entry.source_text)
            } else {
                entry
                    .existing_translation
                    .as_ref()
                    .filter(|target| !is_empty_translation(target))
                    .unwrap_or(&entry.source_text)
            };
            let comparison = compare_placeholders_for_profile(&entry.source_text, target, profile);
            let Some(severity) = comparison.severity else {
                continue;
            };
            let (severity, code, message) = match severity {
                PlaceholderSeverity::Error => (
                    "error",
                    "placeholderError",
                    format!("Placeholderの書式エラー: {namespace}:{key}"),
                ),
                PlaceholderSeverity::Warning => (
                    "warning",
                    "placeholderWarning",
                    format!("引数が表示されない可能性があります: {namespace}:{key}"),
                ),
            };
            issues.push(ExportIssue {
                severity: severity.to_owned(),
                code: code.to_owned(),
                message,
                mod_unit_id: Some(unit.id),
                mod_name: Some(export_unit_label(unit)),
                count: None,
                namespace: Some(namespace.to_owned()),
                key: Some(key),
                display_label,
                source_text: Some(entry.source_text.clone()),
                target_text: Some(target.clone()),
            });
        }
    }
    if units.is_empty() {
        issues.push(ExportIssue {
            severity: "error".to_owned(),
            code: "invalidExportSelection".to_owned(),
            message: "Modpack 固有の項目を選んでください。".to_owned(),
            mod_unit_id: None,
            mod_name: None,
            count: None,
            namespace: None,
            key: None,
            display_label: None,
            source_text: None,
            target_text: None,
        });
    }
    Ok(ExportValidationResult {
        issues,
        counts: crate::commands::project::ProjectCounts {
            total: 0,
            translated: 0,
            missing: 0,
            needs_review: 0,
        },
        is_signed: false,
        minecraft_version_detection: None,
    })
}

fn ftb_quests_warning_label(document_path: &str, pointer: &str) -> String {
    let display_path = document_path
        .split_once("ftbquests/quests/lang/")
        .map(|(_, path)| path)
        .map(|path| path.split_once('/').map_or(path, |(_, relative)| relative))
        .unwrap_or_else(|| document_path.rsplit('/').next().unwrap_or(document_path));
    format!("{display_path} › {pointer}")
}

fn ensure_modpack_deployment_unchanged(
    target: &Path,
    deployed_sha256: &str,
) -> Result<(), AppError> {
    let current_bytes = match fs::read(target) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(modpack_deployment_changed_error(target));
        }
        Err(error) => return Err(export_error(error)),
    };
    if sha256_bytes_hex(&current_bytes) != deployed_sha256 {
        return Err(modpack_deployment_changed_error(target));
    }
    Ok(())
}

fn modpack_deployment_changed_error(target: &Path) -> AppError {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| target.as_os_str().to_string_lossy());
    AppError::new(
        "modpack_file_changed",
        &format!(
            "配置後に変更されたファイル「{name}」は上書きできません。手動で解決してください。"
        ),
        Some(target.to_string_lossy().into_owned()),
    )
}

fn modpack_instance_root(unit: &ModUnitRecord, game_directory: &str) -> Result<PathBuf, AppError> {
    let game_path = PathBuf::from(&unit.original_path);
    if game_directory == "." {
        Ok(game_path)
    } else {
        game_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| project_error("Modpack のインスタンスフォルダーを確認できません"))
    }
}

fn safe_modpack_relative_path(value: &str) -> Result<PathBuf, AppError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(AppError::new(
            "invalid_modpack_path",
            "Modpack のファイルパスが不正です。",
            Some(value.to_owned()),
        ));
    }
    Ok(path.to_path_buf())
}

fn validate_modpack_target_path(
    instance_root: &Path,
    target: &Path,
    create_missing_parents: bool,
) -> Result<(), AppError> {
    let invalid_path =
        || AppError::simple("invalid_modpack_path", "インスタンス外には配置できません。");
    let canonical_root = fs::canonicalize(instance_root).map_err(export_error)?;
    let parent = target.parent().unwrap_or(instance_root);
    let relative_parent = parent
        .strip_prefix(instance_root)
        .map_err(|_| invalid_path())?;
    let mut current = canonical_root.clone();
    for component in relative_parent.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(invalid_path());
        };
        let next = current.join(name);
        match fs::symlink_metadata(&next) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err(invalid_path());
                }
                current = fs::canonicalize(next).map_err(export_error)?;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && create_missing_parents =>
            {
                fs::create_dir(&next).map_err(export_error)?;
                current = fs::canonicalize(next).map_err(export_error)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(export_error(error)),
        }
        if !current.starts_with(&canonical_root) {
            return Err(invalid_path());
        }
    }
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(AppError::simple(
                "invalid_modpack_path",
                "シンボリックリンクには配置できません。",
            ));
        }
        Ok(_) => {
            if !fs::canonicalize(target)
                .map_err(export_error)?
                .starts_with(&canonical_root)
            {
                return Err(invalid_path());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(export_error(error)),
    }
    Ok(())
}

fn write_modpack_output_zip(files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, AppError> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (path, bytes) in files {
        safe_modpack_relative_path(path)?;
        archive.start_file(path, options).map_err(export_error)?;
        archive.write_all(bytes).map_err(export_error)?;
    }
    archive
        .finish()
        .map(|archive| archive.into_inner())
        .map_err(export_error)
}

fn deploy_modpack_file(
    opened: &OpenedProject,
    unit: &ModUnitRecord,
    instance_root: &Path,
    target: &Path,
    bytes: &[u8],
    exported_paths: &Arc<std::sync::Mutex<HashSet<PathBuf>>>,
) -> Result<(), AppError> {
    let repository = opened.repository();
    let target_string = target.to_string_lossy().into_owned();
    validate_modpack_target_path(instance_root, target, true)?;
    let active = repository
        .active_file_deployment(unit.id, &target_string)
        .map_err(|error| project_error(error.to_string()))?;
    if let Some(record) = active.as_ref() {
        ensure_modpack_deployment_unchanged(target, &record.deployed_sha256)?;
    }
    let previous_bytes = if target.is_file() {
        Some(fs::read(target).map_err(export_error)?)
    } else {
        None
    };
    let (existed_before, previous_sha256, backup_path) = if let Some(active) = active.as_ref() {
        (
            active.existed_before,
            active.previous_sha256.clone(),
            active.backup_path.clone(),
        )
    } else if target.is_file() {
        let previous_sha256 = sha256_file_hex(target)?;
        let backup_path = opened
            .directory()
            .join("backups/modpack")
            .join(unit.id.to_string())
            .join(format!("{}.bak", uuid::Uuid::new_v4()));
        fs::create_dir_all(backup_path.parent().expect("backup path has a parent"))
            .map_err(export_error)?;
        copy_atomic_selected(target, &backup_path).map_err(export_error)?;
        (
            true,
            Some(previous_sha256),
            Some(backup_path.to_string_lossy().into_owned()),
        )
    } else {
        (false, None, None)
    };
    if let Some(record) = active.as_ref() {
        ensure_modpack_deployment_unchanged(target, &record.deployed_sha256)?;
    }
    let deployed_sha256 = sha256_bytes_hex(bytes);
    if let Err(error) = atomic_write_selected(target, bytes) {
        return Err(export_error(error));
    }
    let deployment = FileDeploymentRecord {
        id: 0,
        mod_unit_id: unit.id,
        target_path: target_string,
        existed_before,
        previous_sha256,
        backup_path: backup_path.clone(),
        deployed_sha256,
        deployed_at: export_timestamp(),
        restored_at: None,
    };
    if let Err(error) = opened.writer().save_file_deployment(deployment) {
        let rollback = if let Some(previous_bytes) = previous_bytes {
            atomic_write_selected(target, &previous_bytes).map_err(export_error)
        } else {
            fs::remove_file(target)
                .or_else(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
                .map_err(export_error)
        };
        if let Err(rollback_error) = rollback {
            tracing::error!(target = %target.display(), %error, rollback_error = %rollback_error.user_message, "modpack deployment record failed and target rollback also failed");
            return Err(AppError::new(
                "file_deployment_record",
                "配置記録を保存できず、ファイルの復元にも失敗しました。",
                Some(format!("{}; {}", error, rollback_error.user_message)),
            ));
        }
        return Err(project_error(error.to_string()));
    }
    record_exported_path(exported_paths, target)?;
    Ok(())
}

fn sha256_bytes_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

pub(crate) fn restore_modpack_file_core(
    opened: &OpenedProject,
    deployment_id: i64,
    exported_paths: &Arc<std::sync::Mutex<HashSet<PathBuf>>>,
) -> Result<RestoreFileDeploymentOutcome, AppError> {
    let repository = opened.repository();
    let deployment = repository
        .list_file_deployments(false)
        .map_err(|error| project_error(error.to_string()))?
        .into_iter()
        .find(|record| record.id == deployment_id)
        .ok_or_else(|| {
            AppError::simple("deployment_not_found", "復元するファイルが見つかりません。")
        })?;
    let unit = repository
        .list_mod_units()
        .map_err(|error| project_error(error.to_string()))?
        .into_iter()
        .find(|unit| {
            unit.id == deployment.mod_unit_id
                && matches!(
                    unit.source_kind,
                    ModUnitSourceKind::Modpack | ModUnitSourceKind::Shaderpack
                )
        })
        .ok_or_else(|| {
            AppError::simple("deployment_not_found", "復元するファイルが見つかりません。")
        })?;
    let target = PathBuf::from(&deployment.target_path);
    let instance_root = match unit.source_kind {
        ModUnitSourceKind::Modpack => {
            let source = repository
                .mod_unit_source_path(unit.id)
                .map_err(|error| project_error(error.to_string()))?;
            let (_, data) = super::modpack::analyze_modpack_archive(&source)?;
            modpack_instance_root(&unit, &data.scan.game_directory)?
        }
        ModUnitSourceKind::Shaderpack => shaderpack_instance_root(opened)?,
        ModUnitSourceKind::Jar => unreachable!("deployment records belong to file-based units"),
    };
    validate_modpack_target_path(&instance_root, &target, false)?;
    let canonical_root = fs::canonicalize(&instance_root).map_err(export_error)?;
    let canonical_target = canonical_path_with_missing_leaf(&target)?;
    if !canonical_target.starts_with(&canonical_root) {
        return Err(AppError::simple(
            "invalid_deployment_path",
            "インスタンス外のファイルは復元できません。",
        ));
    }
    if deployment.existed_before {
        let backup_path = deployment.backup_path.as_deref().ok_or_else(|| {
            AppError::simple(
                "invalid_deployment_backup",
                "バックアップを確認できません。",
            )
        })?;
        let backup_path = Path::new(backup_path);
        validate_project_backup_path(
            opened.directory(),
            backup_path,
            deployment.previous_sha256.as_deref().unwrap_or_default(),
        )?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(export_error)?;
        }
        ensure_modpack_deployment_unchanged(&target, &deployment.deployed_sha256)?;
        copy_atomic_selected(backup_path, &target).map_err(export_error)?;
    } else {
        ensure_modpack_deployment_unchanged(&target, &deployment.deployed_sha256)?;
        match fs::remove_file(&target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(export_error(error)),
        }
    }
    opened
        .writer()
        .mark_file_deployment_restored(deployment.id, export_timestamp())
        .map_err(|error| project_error(error.to_string()))?;
    if deployment.existed_before {
        record_exported_path(exported_paths, &target)?;
    } else {
        forget_exported_path(exported_paths, &target)?;
    }
    Ok(RestoreFileDeploymentOutcome::Restored {
        target_path: deployment.target_path,
    })
}

fn restore_modified_jar_core(
    opened: &OpenedProject,
    replacement_id: i64,
    confirm_changed: bool,
    exported_paths: &Arc<std::sync::Mutex<HashSet<PathBuf>>>,
) -> Result<RestoreJarOutcome, AppError> {
    let repository = opened.repository();
    let replacement = repository
        .list_jar_replacements(false)
        .map_err(|error| project_error(error.to_string()))?
        .into_iter()
        .find(|record| record.id == replacement_id)
        .ok_or_else(|| {
            AppError::simple("replacement_not_found", "復元するJARが見つかりません。")
        })?;
    let unit = repository
        .list_mod_units()
        .map_err(|error| project_error(error.to_string()))?
        .into_iter()
        .find(|unit| unit.id == replacement.mod_unit_id)
        .ok_or_else(|| {
            AppError::simple("replacement_not_found", "復元するmodが見つかりません。")
        })?;
    if replacement.target_path != unit.original_path {
        return Err(AppError::simple(
            "invalid_replacement_path",
            "JARの保存先を確認できません。",
        ));
    }
    validate_project_backup_path(
        opened.directory(),
        Path::new(&replacement.backup_path),
        &replacement.previous_sha256,
    )?;
    let target = Path::new(&replacement.target_path);
    // 対象JARが削除されても、検証済みバックアップから復元できる。
    // NotFound以外の読込エラーは隠さず、存在する変更済みJARには確認を求める。
    let current_hash = match yc_archive::sha256_file(target) {
        Ok(digest) => Some(sha256_digest_hex(digest)),
        Err(yc_archive::ArchiveError::ArchiveRead(yc_archive::ArchiveReadError::OpenFile {
            source,
            ..
        })) if source.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(export_error(error)),
    };
    if current_hash.is_some_and(|hash| hash != replacement.written_sha256) && !confirm_changed {
        return Ok(RestoreJarOutcome::ConfirmationRequired {
            target_path: replacement.target_path,
        });
    }
    restore_jar_from_backup(Path::new(&replacement.backup_path), target)
        .map_err(export_operation_error)?;
    let restored_at = export_timestamp();
    opened
        .writer()
        .mark_jar_replacement_restored(replacement.id, restored_at)
        .map_err(|error| {
            AppError::new(
                "restore_record",
                "JARは元に戻しましたが、復元記録を保存できませんでした。",
                Some(error.to_string()),
            )
        })?;
    record_exported_path(exported_paths, target)?;
    Ok(RestoreJarOutcome::Restored {
        target_path: replacement.target_path,
    })
}

fn export_unit_label(unit: &ModUnitRecord) -> String {
    if unit.source_kind == ModUnitSourceKind::Modpack {
        return "Modpack 固有（クエスト・KubeJS）".to_owned();
    }
    if unit.source_kind == ModUnitSourceKind::Shaderpack {
        return format!("シェーダーパック: {}", unit.source_filename);
    }

    let name = unit
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&unit.source_filename);
    match unit
        .version
        .as_deref()
        .filter(|version| !version.trim().is_empty())
    {
        Some(version) => format!("{name} {version}"),
        None => name.to_owned(),
    }
}

fn safe_file_component(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|character| {
            if matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            ) || character.is_control()
            {
                '_'
            } else {
                character
            }
        })
        .collect();
    let cleaned =
        cleaned.trim_matches(|character: char| character.is_whitespace() || character == '.');
    if cleaned.is_empty() {
        "mod".to_owned()
    } else {
        cleaned.to_owned()
    }
}

fn mod_export_result(
    mod_unit_id: i64,
    mod_name: String,
    status: &str,
    output_path: Option<String>,
    message: String,
) -> ModExportResult {
    ModExportResult {
        mod_unit_id,
        mod_name,
        status: status.to_owned(),
        output_path,
        message: Some(message),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JarOverwriteDecision {
    Original,
    PreviousWrite,
    Changed,
}

fn jar_overwrite_decision(
    current_sha256: &str,
    source_sha256: &str,
    written_sha256: Option<&str>,
) -> JarOverwriteDecision {
    if current_sha256 == source_sha256 {
        JarOverwriteDecision::Original
    } else if written_sha256 == Some(current_sha256) {
        JarOverwriteDecision::PreviousWrite
    } else {
        JarOverwriteDecision::Changed
    }
}

fn sha256_file_hex(path: &Path) -> Result<String, AppError> {
    let digest = yc_archive::sha256_file(path).map_err(export_error)?;
    Ok(sha256_digest_hex(digest))
}

fn sha256_digest_hex(digest: [u8; 32]) -> String {
    let mut output = String::with_capacity(digest.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn validate_project_backup_path(
    project_directory: &Path,
    backup_path: &Path,
    expected_sha256: &str,
) -> Result<(), AppError> {
    let backup_directory =
        fs::canonicalize(project_directory.join("backups")).map_err(export_error)?;
    let backup_path = fs::canonicalize(backup_path).map_err(export_error)?;
    if !backup_path.starts_with(&backup_directory) || !backup_path.is_file() {
        return Err(AppError::simple(
            "invalid_backup_path",
            "プロジェクト内のバックアップを確認できません。",
        ));
    }
    if sha256_file_hex(&backup_path)? != expected_sha256 {
        return Err(AppError::simple(
            "invalid_backup_hash",
            "元のJARバックアップを確認できません。",
        ));
    }
    Ok(())
}

fn export_timestamp() -> String {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("formatting the current time as RFC3339")
}

fn export_operation_error(error: ExportError) -> AppError {
    match error {
        ExportError::TargetLocked(path) => AppError::new(
            "target_locked",
            "Minecraft が起動中の可能性があります。Minecraft を終了してからもう一度お試しください。",
            Some(path.to_string_lossy().into_owned()),
        ),
        other => export_error(other),
    }
}

fn validate_export_context(
    context: &ExportContext,
    kind: ExportKindInput,
    minecraft_version: Option<&str>,
    namespace: Option<&str>,
    manual_format: Option<ManualPackFormatInput>,
) -> Result<ExportValidationResult, AppError> {
    let kind = if kind == ExportKindInput::ShaderpackZip {
        ExportKind::LanguageFile
    } else {
        export_kind(kind)?
    };
    let base_input = context.input();
    let selected_namespaces: Vec<_> = context
        .namespaces
        .iter()
        .map(|candidate| NamespaceExport {
            namespace: candidate.namespace.clone(),
            included: candidate.included && namespace == Some(candidate.namespace.as_str()),
        })
        .collect();
    let selected_input = ExportInput {
        namespaces: &selected_namespaces,
        ..base_input.clone()
    };
    let input = if kind == ExportKind::LanguageFile {
        &selected_input
    } else {
        &base_input
    };
    let mut issues: Vec<_> = check_export(&ExportValidationRequest {
        input,
        kind,
        minecraft_version,
        manual_pack_format: manual_format.map(|format| ManualPackFormat {
            major: format.major,
            minor: format.minor,
        }),
        original_jar_path: (kind == ExportKind::ModifiedJar)
            .then_some(context.source_jar.as_path()),
    })
    .into_iter()
    .map(|issue| {
        let (namespace, key) = issue
            .message
            .strip_prefix("placeholder mismatch: ")
            .and_then(|detail| detail.split_once(':'))
            .map_or((None, None), |(namespace, key)| {
                (Some(namespace.to_owned()), Some(key.to_owned()))
            });
        let lang_entry = namespace
            .as_deref()
            .zip(key.as_deref())
            .and_then(|(namespace, key)| {
                context
                    .entries
                    .iter()
                    .find(|entry| entry.namespace == namespace && entry.key == key)
            });
        let document_entry =
            namespace
                .as_deref()
                .zip(key.as_deref())
                .and_then(|(namespace, key)| {
                    context
                        .document_entries
                        .iter()
                        .find(|entry| entry.namespace == namespace && entry.key == key)
                });
        let source_text = lang_entry
            .and_then(|entry| entry.source_text.as_deref())
            .or_else(|| document_entry.and_then(|entry| entry.source_text.as_deref()));
        let target_text = lang_entry
            .map(|entry| entry.target_text.as_str())
            .or_else(|| document_entry.and_then(|entry| entry.target_text.as_deref()));
        let placeholder_comparison = (issue.code == ValidationIssueCode::PlaceholderWarning
            && context.source_kind != ModUnitSourceKind::Shaderpack)
            .then(|| {
                let source = source_text?;
                let target = target_text?;
                let profile = if let Some(entry) = document_entry {
                    match entry.entry_kind {
                        TranslationEntryKind::Patchouli => PlaceholderProfile::Patchouli,
                        TranslationEntryKind::Ftbquests => PlaceholderProfile::FtbQuests,
                        TranslationEntryKind::Lang => PlaceholderProfile::Lang,
                    }
                } else if lang_entry
                    .is_some_and(|entry| entry.flags.contains(&EntryFlag::PatchouliText))
                {
                    PlaceholderProfile::Patchouli
                } else {
                    PlaceholderProfile::Lang
                };
                Some(compare_placeholders_for_profile(source, target, profile))
            })
            .flatten();
        let (severity, code, message) = match placeholder_comparison
            .as_ref()
            .and_then(|comparison| comparison.severity)
        {
            Some(PlaceholderSeverity::Error) => (
                "error",
                "placeholderError",
                format!(
                    "Placeholderの書式エラー: {}",
                    issue.message.trim_start_matches("placeholder mismatch: ")
                ),
            ),
            Some(PlaceholderSeverity::Warning) => (
                "warning",
                "placeholderWarning",
                format!(
                    "引数が表示されない可能性があります: {}",
                    issue.message.trim_start_matches("placeholder mismatch: ")
                ),
            ),
            None => (
                match issue.severity {
                    ValidationSeverity::Warning => "warning",
                    ValidationSeverity::Error => "error",
                },
                issue_code(&issue.code),
                issue_message(&issue.code, &issue.message),
            ),
        };
        let count = (issue.code == ValidationIssueCode::UntranslatedEntries)
            .then(|| issue.message.split_whitespace().next()?.parse().ok())
            .flatten();
        ExportIssue {
            severity: severity.to_owned(),
            code: code.to_owned(),
            message,
            mod_unit_id: Some(context.mod_unit_id),
            mod_name: Some(context.mod_name.clone()),
            count,
            namespace,
            key,
            display_label: None,
            source_text: source_text.map(str::to_owned),
            target_text: target_text.map(str::to_owned),
        }
    })
    .collect();
    issues.extend(
        context
            .duplicate_key_warnings
            .iter()
            .map(|(path, message)| ExportIssue {
                severity: "warning".to_owned(),
                code: "duplicateKeys".to_owned(),
                message: format!("{path}: {message}"),
                mod_unit_id: Some(context.mod_unit_id),
                mod_name: Some(context.mod_name.clone()),
                count: None,
                namespace: None,
                key: None,
                display_label: None,
                source_text: None,
                target_text: None,
            }),
    );
    if !context
        .namespaces
        .iter()
        .any(|candidate| candidate.included)
    {
        issues.push(ExportIssue {
            severity: "error".to_owned(),
            code: "invalidExportSelection".to_owned(),
            message: "翻訳対象のNamespaceを選んでください。".to_owned(),
            mod_unit_id: Some(context.mod_unit_id),
            mod_name: Some(context.mod_name.clone()),
            count: None,
            namespace: None,
            key: None,
            display_label: None,
            source_text: None,
            target_text: None,
        });
    }
    if kind == ExportKind::LanguageFile
        && !context
            .namespaces
            .iter()
            .any(|candidate| candidate.included && namespace == Some(candidate.namespace.as_str()))
    {
        issues.push(ExportIssue {
            severity: "error".to_owned(),
            code: "invalidExportSelection".to_owned(),
            message: "書き出すNamespaceを選び直してください。".to_owned(),
            mod_unit_id: Some(context.mod_unit_id),
            mod_name: Some(context.mod_name.clone()),
            count: None,
            namespace: None,
            key: None,
            display_label: None,
            source_text: None,
            target_text: None,
        });
    }
    Ok(ExportValidationResult {
        issues,
        counts: context.counts.clone(),
        is_signed: context.is_signed,
        minecraft_version_detection: context.minecraft_version_detection.clone(),
    })
}

pub(crate) fn export_language_file_core(
    opened: &OpenedProject,
    output_path: &Path,
    namespace: &str,
) -> Result<PathBuf, AppError> {
    let units = opened
        .repository()
        .list_mod_units()
        .map_err(|error| project_error(error.to_string()))?;
    let mut first_output = None;
    for unit in &units {
        let context = export_context(opened, unit.id)?;
        let selected_namespaces: Vec<_> = context
            .namespaces
            .iter()
            .map(|candidate| NamespaceExport {
                namespace: candidate.namespace.clone(),
                included: candidate.included && candidate.namespace == namespace,
            })
            .collect();
        if !selected_namespaces
            .iter()
            .any(|candidate| candidate.included)
        {
            continue;
        }
        if unit.source_kind == ModUnitSourceKind::Shaderpack {
            let namespace = selected_namespaces
                .iter()
                .find(|candidate| candidate.included)
                .map(|candidate| candidate.namespace.as_str())
                .expect("a selected namespace exists");
            let (_, bytes) = shaderpack_lang_export(&context, namespace)?;
            let unit_output = output_path_for_mod(output_path, unit.id, units.len());
            ensure_output_is_safe(&context.source_jar, &unit_output)?;
            ensure_file_extension(&unit_output, "lang")?;
            atomic_write_selected(&unit_output, &bytes).map_err(export_error)?;
            first_output.get_or_insert_with(|| unit_output.clone());
            continue;
        }
        ensure_output_is_safe(&context.source_jar, output_path)?;
        let input = ExportInput {
            namespaces: &selected_namespaces,
            ..context.input()
        };
        let artifacts = yc_export::create_language_files(&input).map_err(export_error)?;
        let documents = yc_export::create_patchouli_document_files(&input).map_err(export_error)?;
        if artifacts.is_empty() && documents.is_empty() {
            continue;
        }
        let output_root = if let Some(artifact) = artifacts.first() {
            let extension = artifact
                .file_name
                .rsplit_once('.')
                .map(|(_, extension)| extension)
                .unwrap_or_default();
            let artifact_output = output_path.with_extension(extension);
            let unit_output = output_path_for_mod(&artifact_output, unit.id, units.len());
            ensure_file_extension(&unit_output, extension)?;
            atomic_write_selected(&unit_output, &artifact.bytes).map_err(export_error)?;
            first_output.get_or_insert_with(|| unit_output.clone());
            if units.len() > 1 {
                let parent = unit_output.parent().unwrap_or(Path::new("."));
                let stem = unit_output
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("export");
                parent.join(stem)
            } else {
                unit_output.parent().unwrap_or(Path::new(".")).to_path_buf()
            }
        } else {
            let parent = output_path.parent().unwrap_or(Path::new("."));
            if units.len() <= 1 {
                parent.to_path_buf()
            } else {
                let stem = output_path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("export");
                parent.join(format!("{stem}-{unit_id}", unit_id = unit.id))
            }
        };
        fs::create_dir_all(&output_root).map_err(export_error)?;
        for document in &documents {
            let document_output = output_root.join(&document.path);
            ensure_output_is_safe(&context.source_jar, &document_output)?;
            if let Some(parent) = document_output.parent() {
                fs::create_dir_all(parent).map_err(export_error)?;
            }
        }
        yc_export::write_patchouli_document_files(&output_root, &documents, false)
            .map_err(export_error)?;
        if first_output.is_none() {
            first_output = documents
                .first()
                .map(|document| output_root.join(&document.path));
        }
    }
    first_output.ok_or_else(|| {
        AppError::simple(
            "invalid_export_selection",
            "書き出すNamespaceを選び直してください。",
        )
    })
}

fn export_context(opened: &OpenedProject, mod_unit_id: i64) -> Result<ExportContext, AppError> {
    let repository = opened.repository();
    let unit = repository
        .list_mod_units()
        .map_err(|error| project_error(error.to_string()))?
        .into_iter()
        .find(|unit| unit.id == mod_unit_id)
        .ok_or_else(|| project_error("modが見つかりません"))?;
    let source_jar = repository
        .mod_unit_source_path(mod_unit_id)
        .map_err(|error| project_error(error.to_string()))?;
    let analysis = match unit.source_kind {
        ModUnitSourceKind::Jar => {
            yc_analysis::analyze_jar(&source_jar, &CancellationToken::default(), |_| {}).map_err(
                |error| {
                    AppError::new(
                        "archive_read",
                        "JARを読み込めませんでした。プロジェクトを開き直してください。",
                        Some(error.to_string()),
                    )
                },
            )?
        }
        ModUnitSourceKind::Modpack => super::modpack::analyze_modpack_archive(&source_jar)?.0,
        ModUnitSourceKind::Shaderpack => yc_analysis::analyze_shaderpack(
            &source_jar,
            &shaderpack_namespace(&unit.source_filename),
        )
        .map_err(|error| {
            AppError::new(
                "shaderpack_read",
                "シェーダーパックを読み込めませんでした。プロジェクトを開き直してください。",
                Some(error.to_string()),
            )
        })?,
    };
    let saved_settings = project_review_settings_for_unit(opened, mod_unit_id)?;
    let settings: ReviewSettings =
        review_settings_from_record(&saved_settings, &analysis, mod_unit_id);
    let mut entries = merge_for_review(&analysis, &settings).entries;
    let saved_entries = repository
        .list_entries(Some(mod_unit_id))
        .map_err(|error| project_error(error.to_string()))?;
    overlay_saved_entries(&mut entries, saved_entries);
    let saved_document_entries = repository
        .list_document_entries(Some(mod_unit_id))
        .map_err(|error| project_error(error.to_string()))?;
    let patchouli_documents = patchouli_document_exports(&source_jar, &saved_document_entries)?;
    let summary = repository
        .entry_summary(Some(mod_unit_id))
        .map_err(|error| project_error(error.to_string()))?;
    let counts = crate::commands::project::ProjectCounts {
        total: summary.total,
        translated: summary.translated,
        missing: summary.total.saturating_sub(summary.translated),
        needs_review: summary.needs_review,
    };
    let mut placeholder_warnings: Vec<_> = entries
        .iter_mut()
        .filter_map(|entry| {
            let mismatch = !is_empty_translation(&entry.target_text)
                && entry.source_text.as_deref().is_some_and(|source| {
                    if unit.source_kind == ModUnitSourceKind::Shaderpack {
                        yc_validation::shader_format_warning(source, &entry.target_text)
                    } else {
                        let profile = if entry.flags.contains(&EntryFlag::PatchouliText) {
                            PlaceholderProfile::Patchouli
                        } else {
                            PlaceholderProfile::Lang
                        };
                        !compare_placeholders_for_profile(source, &entry.target_text, profile)
                            .is_valid
                    }
                });
            // Exportでは保存済みflagではなく、今のsource/targetから警告を再計算する。
            entry
                .flags
                .retain(|flag| flag != &EntryFlag::PlaceholderError);
            mismatch.then(|| PlaceholderWarning {
                namespace: entry.namespace.clone(),
                key: entry.key.clone(),
            })
        })
        .collect();
    for entry in &saved_document_entries {
        let profile = match entry.entry_kind {
            TranslationEntryKind::Patchouli => PlaceholderProfile::Patchouli,
            TranslationEntryKind::Ftbquests => PlaceholderProfile::FtbQuests,
            TranslationEntryKind::Lang => continue,
        };
        if let (Some(source), Some(target)) = (&entry.source_text, &entry.target_text)
            && !is_empty_translation(target)
            && !compare_placeholders_for_profile(source, target, profile).is_valid
        {
            placeholder_warnings.push(PlaceholderWarning {
                namespace: entry.namespace.clone(),
                key: entry.key.clone(),
            });
        }
    }
    let source_locale = if settings.source_locale.is_empty() {
        analysis
            .result
            .recommended_source_locale
            .clone()
            .unwrap_or_default()
    } else {
        settings.source_locale.clone()
    };
    let malformed_translation_files = analysis
        .result
        .warnings
        .iter()
        .filter(|warning| {
            warning
                .message
                .starts_with("言語ファイルを解析できませんでした")
                || warning
                    .message
                    .starts_with("言語ファイルを読み取れませんでした")
        })
        .map(|warning| warning.path.clone())
        .collect();
    let language_files = discovered_languages(&analysis);
    let duplicate_key_warnings = analysis
        .result
        .warnings
        .iter()
        .filter(|warning| {
            warning.message.starts_with("重複キー")
                && language_files.iter().any(|file| {
                    file.archive_path == warning.path
                        && (file.normalized_locale == normalize_locale(&source_locale)
                            || file.normalized_locale == "ja_jp")
                })
        })
        .map(|warning| (warning.path.clone(), warning.message.clone()))
        .collect();
    let metadata_ranges: Vec<_> = analysis
        .result
        .mods
        .iter()
        .filter_map(|mod_info| mod_info.minecraft_version.as_deref())
        .collect();
    let minecraft_version_detection = if unit.source_kind == ModUnitSourceKind::Shaderpack {
        None
    } else {
        detect_minecraft_version_for_project(&source_jar, &metadata_ranges)
    };
    let legacy_parse_escapes_namespaces = if unit.source_kind == ModUnitSourceKind::Shaderpack {
        Vec::new()
    } else {
        legacy_parse_escapes_namespaces(
            &source_jar,
            &language_files,
            &source_locale,
            &settings.included_namespaces,
        )?
    };
    let mut namespaces: Vec<_> = analysis
        .result
        .namespaces
        .iter()
        .map(|namespace| NamespaceExport {
            namespace: namespace.namespace.clone(),
            included: settings.included_namespaces.contains(&namespace.namespace),
        })
        .collect();
    if saved_document_entries
        .iter()
        .any(|entry| entry.entry_kind == TranslationEntryKind::Ftbquests)
    {
        namespaces.push(NamespaceExport {
            namespace: "ftbquests".to_owned(),
            included: true,
        });
    }
    Ok(ExportContext {
        mod_unit_id,
        source_kind: unit.source_kind,
        mod_name: export_unit_label(&unit),
        source_jar,
        source_jar_filename: unit.source_filename,
        mods: analysis
            .result
            .mods
            .iter()
            .map(|mod_info| ResourcePackMod {
                name: mod_info.name.clone(),
                version: mod_info.version.clone(),
            })
            .collect(),
        entries,
        document_entries: saved_document_entries,
        namespaces,
        language_files,
        legacy_parse_escapes_namespaces,
        placeholder_warnings,
        patchouli_documents,
        counts,
        malformed_translation_files,
        duplicate_key_warnings,
        source_locale,
        is_signed: analysis.result.is_signed,
        minecraft_version_detection,
    })
}

fn patchouli_document_exports(
    source_jar: &Path,
    entries: &[DocumentProjectEntry],
) -> Result<Vec<PatchouliDocumentExport>, AppError> {
    let mut grouped =
        BTreeMap::<(String, String), (String, Vec<PatchouliDocumentTranslation>)>::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.entry_kind == TranslationEntryKind::Patchouli)
    {
        let (container_path, document_path) = entry.document_path.split_once("!/").map_or(
            (String::new(), entry.document_path.as_str()),
            |(container, path)| (container.to_owned(), path),
        );
        let target_path =
            yc_language::patchouli_target_document_path(document_path, "en_us", "ja_jp")
                .ok_or_else(|| {
                    export_error(format!(
                        "Patchouli文書のパスを確認できません: {document_path}"
                    ))
                })?;
        let translations = grouped
            .entry((container_path, document_path.to_owned()))
            .or_insert_with(|| (target_path, Vec::new()));
        translations.1.push(PatchouliDocumentTranslation {
            key: entry.key.clone(),
            pointer: entry.document_pointer.clone(),
            source_text: entry.source_text.clone().unwrap_or_default(),
            target_text: entry.target_text.clone().unwrap_or_default(),
        });
    }
    if grouped.is_empty() {
        return Ok(Vec::new());
    }

    let nested_paths: Vec<_> = grouped
        .keys()
        .map(|(container, _)| container.as_str())
        .filter(|container| !container.is_empty())
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut archive = yc_archive::Archive::open(source_jar).map_err(export_error)?;
    let mut nested_archives = if nested_paths.is_empty() {
        Vec::new()
    } else {
        archive
            .discover_nested(&nested_paths)
            .map_err(export_error)?
    };

    let mut documents = Vec::with_capacity(grouped.len());
    for ((container_path, source_path), (target_path, translations)) in grouped {
        let source_bytes = if container_path.is_empty() {
            archive.read_entry(&source_path, yc_archive::EntryKind::Text)
        } else {
            let Some(nested) = nested_archives
                .iter_mut()
                .find(|nested| nested.path == container_path)
                .and_then(|nested| nested.archive.as_mut())
            else {
                return Err(export_error(format!(
                    "同梱JARを読み込めません: {container_path}"
                )));
            };
            nested.read_entry(&source_path, yc_archive::EntryKind::Text)
        }
        .map_err(export_error)?;
        let namespace = source_path
            .strip_prefix("assets/")
            .and_then(|path| path.split_once('/'))
            .map(|(namespace, _)| namespace)
            .ok_or_else(|| {
                export_error(format!(
                    "Patchouli文書のnamespaceを確認できません: {source_path}"
                ))
            })?
            .to_owned();
        documents.push(PatchouliDocumentExport {
            namespace,
            container_path,
            source_path,
            target_path,
            source_bytes,
            translations,
        });
    }
    Ok(documents)
}

fn legacy_parse_escapes_namespaces(
    source_jar: &Path,
    language_files: &[yc_language::DiscoveredLanguageFile],
    source_locale: &str,
    included_namespaces: &[String],
) -> Result<Vec<String>, AppError> {
    // Forgeの拡張モードはlocale概要には保存されないため、選択中の原文ファイルを読み直して
    // 推測せずに書き出しへ引き継ぐ。
    let mut namespaces: Vec<_> = language_files
        .iter()
        // 解析で読み飛ばされた対象外namespaceを再読込すると、正常な対象のExportも失敗する。
        .filter(|file| included_namespaces.contains(&file.namespace))
        .map(|file| file.namespace.as_str())
        .collect();
    namespaces.sort_unstable();
    namespaces.dedup();
    let normalized_source_locale = yc_language::normalize_locale(source_locale);
    let selected_files: Vec<_> = namespaces
        .into_iter()
        .filter_map(|namespace| {
            language_files.iter().find(|file| {
                file.namespace == namespace
                    && file.normalized_locale == normalized_source_locale
                    && file.format == yc_language::LanguageFileFormat::LegacyLang
            })
        })
        .collect();
    if selected_files.is_empty() {
        return Ok(Vec::new());
    }

    let nested_paths: Vec<_> = selected_files
        .iter()
        .filter(|file| !file.container_path.is_empty())
        .map(|file| file.container_path.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut archive = yc_archive::Archive::open(source_jar).map_err(|error| {
        AppError::new(
            "archive_read",
            "JAR内の言語ファイルを読み込めませんでした。プロジェクトを開き直してください。",
            Some(error.to_string()),
        )
    })?;
    let mut nested_archives = if nested_paths.is_empty() {
        Vec::new()
    } else {
        archive.discover_nested(&nested_paths).map_err(|error| {
            AppError::new(
                "archive_read",
                "JAR内の言語ファイルを読み込めませんでした。プロジェクトを開き直してください。",
                Some(error.to_string()),
            )
        })?
    };

    let mut namespaces = Vec::new();
    for file in selected_files {
        let bytes = if file.container_path.is_empty() {
            archive.read_entry(&file.archive_path, yc_archive::EntryKind::Text)
        } else {
            let Some(nested) = nested_archives
                .iter_mut()
                .find(|nested| nested.path == file.container_path)
                .and_then(|nested| nested.archive.as_mut())
            else {
                tracing::warn!(
                    path = %file.container_path,
                    "legacy language mode could not be inspected in nested JAR"
                );
                continue;
            };
            nested.read_entry(&file.archive_path, yc_archive::EntryKind::Text)
        }
        .map_err(|error| {
            AppError::new(
                "archive_read",
                "JAR内の言語ファイルを読み込めませんでした。プロジェクトを開き直してください。",
                Some(error.to_string()),
            )
        })?;

        if yc_language::legacy_language_uses_parse_escapes(&bytes) {
            namespaces.push(file.namespace.clone());
        }
    }
    namespaces.sort();
    namespaces.dedup();
    Ok(namespaces)
}

fn overlay_saved_entries(entries: &mut [TranslationEntry], saved_entries: Vec<ProjectEntry>) {
    let mut by_key: std::collections::HashMap<_, _> = saved_entries
        .into_iter()
        .map(|entry| ((entry.namespace.clone(), entry.key.clone()), entry))
        .collect();
    for entry in entries {
        if let Some(saved) = by_key.remove(&(entry.namespace.clone(), entry.key.clone())) {
            entry.source_text = saved.source_text;
            entry.source_locale = saved.source_locale;
            entry.target_text = saved.target_text.unwrap_or_default();
            entry.target_original_text = saved.target_original_text;
            entry.translated_source_text = saved.translated_source_text;
            entry.origin = saved.origin;
            entry.sort_index = saved.sort_index;
            let has_patchouli_text = entry.flags.contains(&EntryFlag::PatchouliText);
            entry.flags = saved.flags;
            if has_patchouli_text && !entry.flags.contains(&EntryFlag::PatchouliText) {
                entry.flags.push(EntryFlag::PatchouliText);
            }
        }
    }
}

fn export_kind(kind: ExportKindInput) -> Result<ExportKind, AppError> {
    match kind {
        ExportKindInput::LanguageFile => Ok(ExportKind::LanguageFile),
        ExportKindInput::ResourcePack => Ok(ExportKind::ResourcePack),
        ExportKindInput::ModifiedJar => Ok(ExportKind::ModifiedJar),
        ExportKindInput::ShaderpackZip => Ok(ExportKind::LanguageFile),
        ExportKindInput::ModpackFiles => Err(AppError::simple(
            "invalid_export_kind",
            "Modpack ファイルの検証方法が不正です。",
        )),
    }
}

fn issue_code(code: &ValidationIssueCode) -> &'static str {
    match code {
        ValidationIssueCode::MalformedTranslationFile => "malformedTranslationFile",
        ValidationIssueCode::PlaceholderWarning => "placeholderWarning",
        ValidationIssueCode::UntranslatedEntries => "untranslatedEntries",
        ValidationIssueCode::DuplicateKeys => "duplicateKeys",
        ValidationIssueCode::UnsupportedTargetFormat => "unsupportedTargetFormat",
        ValidationIssueCode::MissingMinecraftVersion => "missingMinecraftVersion",
        ValidationIssueCode::UnknownPackFormat => "unknownPackFormat",
        ValidationIssueCode::UnsafeTargetPath => "unsafeTargetPath",
        ValidationIssueCode::SignedJar => "signedJar",
        ValidationIssueCode::NestedJarAlreadyHasTarget => "nestedJarAlreadyHasTarget",
        ValidationIssueCode::SourceJarUnavailable => "sourceJarUnavailable",
    }
}

fn issue_message(code: &ValidationIssueCode, detail: &str) -> String {
    match code {
        ValidationIssueCode::MalformedTranslationFile => {
            format!(
                "言語ファイルを読み込めませんでした: {}",
                detail.trim_start_matches("translation file could not be parsed: ")
            )
        }
        ValidationIssueCode::PlaceholderWarning => format!(
            "Placeholderが一致しません: {}",
            detail.trim_start_matches("placeholder mismatch: ")
        ),
        ValidationIssueCode::UntranslatedEntries => {
            let count = detail.split_whitespace().next().unwrap_or_default();
            format!("{count}件は未翻訳です")
        }
        ValidationIssueCode::DuplicateKeys => format!(
            "重複する翻訳キーがあります: {}",
            detail.trim_start_matches("duplicate translation keys in ")
        ),
        ValidationIssueCode::UnsupportedTargetFormat => {
            "この言語ファイル形式には対応していません。".to_owned()
        }
        ValidationIssueCode::MissingMinecraftVersion => {
            "Minecraft バージョンを選んでください。".to_owned()
        }
        ValidationIssueCode::UnknownPackFormat => {
            "リソースパックの形式を判定できません。Minecraft バージョンを確認してください。"
                .to_owned()
        }
        ValidationIssueCode::UnsafeTargetPath => {
            "言語ファイルの保存パスを確認してください。".to_owned()
        }
        ValidationIssueCode::SignedJar => {
            "署名済みのJARは変更できません。Resource Packを選んでください。".to_owned()
        }
        ValidationIssueCode::NestedJarAlreadyHasTarget => {
            "同梱Modに日本語ファイルが含まれています。Resource Packの使用をおすすめします。"
                .to_owned()
        }
        ValidationIssueCode::SourceJarUnavailable => {
            "元のJARを読み込めません。プロジェクトを開き直してください。".to_owned()
        }
    }
}

fn ensure_has_included_namespaces(context: &ExportContext) -> Result<(), AppError> {
    if context
        .namespaces
        .iter()
        .any(|namespace| namespace.included)
    {
        Ok(())
    } else {
        Err(AppError::simple(
            "invalid_export_selection",
            "翻訳対象のNamespaceを選んでください。",
        ))
    }
}

fn ensure_file_extension(path: &Path, expected: &str) -> Result<(), AppError> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
    {
        Ok(())
    } else {
        Err(AppError::simple(
            "invalid_output_path",
            &format!("保存先の拡張子を .{expected} にしてください。"),
        ))
    }
}

fn ensure_output_is_safe(source: &Path, output: &Path) -> Result<(), AppError> {
    // シンボリックリンク経由も含めて比較し、翻訳元JARを上書きして復元不能にしない。
    if canonical_path_with_missing_leaf(source)? == canonical_path_with_missing_leaf(output)? {
        return Err(AppError::simple(
            "source_overwrite",
            "元のJARを保存先に選べません。別の保存先を選んでください。",
        ));
    }
    if output.file_name().is_none() {
        return Err(AppError::simple(
            "invalid_output_path",
            "保存先ファイル名を選んでください。",
        ));
    }
    Ok(())
}

fn ensure_output_is_within_directory(directory: &Path, output: &Path) -> Result<(), AppError> {
    let canonical_directory = fs::canonicalize(directory).map_err(export_error)?;
    let canonical_output = canonical_path_with_missing_leaf(output)?;
    if !canonical_output.starts_with(&canonical_directory) {
        return Err(AppError::simple(
            "invalid_output_path",
            "保存先フォルダーの外には書き出せません。",
        ));
    }
    Ok(())
}

fn normalized_shaderpack_path(path: &Path) -> Result<String, AppError> {
    let mut missing = Vec::new();
    let mut existing = path;
    while !existing.exists() {
        missing.push(existing.file_name().ok_or_else(|| {
            AppError::simple("invalid_output_path", "保存先ファイル名を選んでください。")
        })?);
        existing = existing
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
    }
    let mut normalized = fs::canonicalize(existing).map_err(export_error)?;
    for component in missing.into_iter().rev() {
        normalized.push(component);
    }
    Ok(normalized.to_string_lossy().to_lowercase())
}

fn original_jar_is_in_instance_mods(instance_path: &Path, original_path: &Path) -> bool {
    let Ok(instance_root) = fs::canonicalize(instance_path) else {
        return false;
    };
    let Ok(mods_directory) = fs::canonicalize(instance_root.join("mods")) else {
        return false;
    };
    let Ok(original_jar) = fs::canonicalize(original_path) else {
        return false;
    };
    if !mods_directory.is_dir() || !original_jar.is_file() {
        return false;
    }
    canonical_path_is_within(&mods_directory, &instance_root)
        && canonical_path_is_within(&original_jar, &mods_directory)
}

#[cfg(windows)]
fn canonical_path_is_within(path: &Path, directory: &Path) -> bool {
    let directory_components: Vec<_> = directory
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let path_components: Vec<_> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    path_components.len() > directory_components.len()
        && path_components.starts_with(&directory_components)
}

#[cfg(not(windows))]
fn canonical_path_is_within(path: &Path, directory: &Path) -> bool {
    path != directory && path.starts_with(directory)
}

fn canonical_path_with_missing_leaf(path: &Path) -> Result<PathBuf, AppError> {
    if path.exists() {
        return fs::canonicalize(path).map_err(export_error);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent).map_err(export_error)?;
    Ok(parent.join(path.file_name().ok_or_else(|| {
        AppError::simple("invalid_output_path", "保存先ファイル名を選んでください。")
    })?))
}

fn atomic_write_selected(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = parent.join(format!(".yakucraft-export-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn atomic_write_selected_if_absent(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = parent.join(format!(".yakucraft-export-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        publish_file_without_replacing(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn copy_atomic_selected(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = parent.join(format!(".yakucraft-export-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut input = File::open(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        std::io::copy(&mut input, &mut output)?;
        output.flush()?;
        output.sync_all()?;
        drop(output);
        replace_file(&temporary, destination)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn replace_file(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // 保存先の既存ファイルは、完成した一時ファイルとの置換が成功するまで保持する。
        let replaced = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if replaced == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        fs::rename(source, destination)
    }
}

fn publish_file_without_replacing(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // Omitting MOVEFILE_REPLACE_EXISTING makes publishing fail if another file appeared meanwhile.
        let moved = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        // A hard link atomically claims the destination without replacing an existing file.
        fs::hard_link(source, destination)?;
        fs::remove_file(source)
    }
}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

fn record_exported_path(
    exported_paths: &Arc<std::sync::Mutex<std::collections::HashSet<PathBuf>>>,
    path: &Path,
) -> Result<(), AppError> {
    let canonical = fs::canonicalize(path).map_err(export_error)?;
    exported_paths
        .lock()
        .map_err(|_| state_error("exported paths state"))?
        .insert(canonical);
    Ok(())
}

fn forget_exported_path(
    exported_paths: &Arc<std::sync::Mutex<std::collections::HashSet<PathBuf>>>,
    path: &Path,
) -> Result<(), AppError> {
    let canonical = canonical_path_with_missing_leaf(path)?;
    exported_paths
        .lock()
        .map_err(|_| state_error("exported paths state"))?
        .remove(&canonical);
    Ok(())
}

fn export_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        "export",
        "ファイルを書き出せませんでした。保存先を確認してもう一度お試しください。",
        Some(error.to_string()),
    )
}

fn project_error(detail: impl Into<String>) -> AppError {
    AppError::new(
        "database",
        "プロジェクトを読み込めませんでした。アプリを再起動してもう一度お試しください。",
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

#[cfg(test)]
mod overwrite_decision_tests {
    use super::{JarOverwriteDecision, jar_overwrite_decision};

    #[test]
    fn original_hash_allows_initial_replacement() {
        assert_eq!(
            jar_overwrite_decision("source", "source", None),
            JarOverwriteDecision::Original
        );
    }

    #[test]
    fn previous_yakucraft_hash_allows_replacement_and_other_hash_skips() {
        assert_eq!(
            jar_overwrite_decision("written", "source", Some("written")),
            JarOverwriteDecision::PreviousWrite
        );
        assert_eq!(
            jar_overwrite_decision("updated", "source", Some("written")),
            JarOverwriteDecision::Changed
        );
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::overlay_saved_entries;
    use yc_language::{EntryFlag, TranslationEntry, TranslationOrigin};
    use yc_project::ProjectEntry;

    #[test]
    fn saved_entry_flags_keep_patchouli_classification_from_analysis() {
        let mut entries = vec![TranslationEntry {
            namespace: "fixture".to_owned(),
            key: "book.landing".to_owned(),
            source_text: Some("Welcome $(bold)world$(/bold)".to_owned()),
            source_locale: Some("en_us".to_owned()),
            target_text: String::new(),
            target_original_text: None,
            translated_source_text: None,
            origin: TranslationOrigin::Missing,
            flags: vec![EntryFlag::PatchouliText],
            sort_index: 0,
        }];
        let saved = ProjectEntry {
            id: 1,
            mod_unit_id: 1,
            namespace: "fixture".to_owned(),
            key: "book.landing".to_owned(),
            source_text: Some("Welcome $(bold)world$(/bold)".to_owned()),
            source_locale: Some("en_us".to_owned()),
            target_text: Some("ようこそ 世界".to_owned()),
            target_original_text: None,
            translated_source_text: None,
            origin: TranslationOrigin::Edited,
            sort_index: 0,
            flags: Vec::new(),
        };

        overlay_saved_entries(&mut entries, vec![saved]);

        assert!(entries[0].flags.contains(&EntryFlag::PatchouliText));
        assert_eq!(entries[0].target_text, "ようこそ 世界");
    }
}

#[cfg(test)]
mod export_path_regression_tests {
    use std::{
        fs,
        io::Write,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        ExportKindInput, MergedPackExportOutcome, MergedPackExportRequest, RestoreJarOutcome,
        active_project_for_id, benchmark_export_selected_modified_jars, export_context,
        export_language_file_and_record, export_selected_language_files_core,
        export_selected_merged_resource_pack_core, legacy_parse_escapes_namespaces,
        original_jar_is_in_instance_mods, restore_modified_jar_core, sha256_file_hex,
        validate_project_core_for_units,
    };
    use crate::{commands::project::create_project_from_sources_core, paths::AppPaths};
    use yc_analysis::CancellationToken;
    use yc_project::{EntryUpdate, JarReplacementRecord, OpenedProject, UpdateOrigin};
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn create() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "yakucraft-export-path-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("temporary directory should be created");
            Self(path)
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn export_project_fixture() -> (TemporaryDirectory, OpenedProject, i64) {
        export_project_fixture_with_instance(false)
    }

    fn export_project_fixture_with_instance(
        with_instance: bool,
    ) -> (TemporaryDirectory, OpenedProject, i64) {
        let temporary = TemporaryDirectory::create();
        let source = temporary.0.join("fixture.jar");
        let file = fs::File::create(&source).expect("fixture JAR should be created");
        let mut jar = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (path, contents) in [
            (
                "fabric.mod.json",
                r#"{"schemaVersion":1,"id":"fixture","name":"Fixture Mod","version":"1.0.0"}"#,
            ),
            (
                "assets/fixture/lang/fr_fr.json",
                r#"{"fixture.message":"Source text"}"#,
            ),
        ] {
            jar.start_file(path, options)
                .expect("fixture ZIP entry should start");
            jar.write_all(contents.as_bytes())
                .expect("fixture ZIP entry should be written");
        }
        jar.finish().expect("fixture JAR should be finalized");

        let paths = AppPaths::create(temporary.0.join("app-data"))
            .expect("application data directories should be created");
        let instance = temporary.0.join("instance");
        let created = create_project_from_sources_core(
            &paths,
            &[source],
            with_instance.then_some(instance.as_path()),
            false,
            &CancellationToken::default(),
            |_, _, _, _| {},
        )
        .expect("fixture JAR should create a Project");
        let mod_unit_id = created
            .opened
            .repository()
            .list_mod_units()
            .expect("mod units should be listed")
            .first()
            .expect("fixture should contain one mod unit")
            .id;
        let mut settings = created.analyses[&mod_unit_id].default_review_settings();
        settings.source_locale = "en_us".to_owned();
        let opened = created.opened;
        opened
            .writer()
            .save_review_settings(mod_unit_id, settings, true)
            .expect("default Review settings should be saved");
        let entry = opened
            .repository()
            .list_entries(Some(mod_unit_id))
            .expect("fixture entries should be listed")
            .into_iter()
            .next()
            .expect("fixture should contain one translation entry");
        opened
            .writer()
            .update_entry(EntryUpdate {
                id: entry.id,
                target_text: "翻訳".to_owned(),
                origin: UpdateOrigin::Manual,
            })
            .expect("fixture translation should be saved");
        (temporary, opened, mod_unit_id)
    }

    fn write_in_place_fixture_jar(path: &Path, mod_id: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = fs::File::create(path).unwrap();
        let mut jar = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let metadata =
            format!(r#"{{"schemaVersion":1,"id":"{mod_id}","name":"{mod_id}","version":"1.0.0"}}"#);
        jar.start_file("fabric.mod.json", options).unwrap();
        jar.write_all(metadata.as_bytes()).unwrap();
        jar.start_file(format!("assets/{mod_id}/lang/fr_fr.json"), options)
            .unwrap();
        jar.write_all(br#"{"fixture.message":"Source text"}"#)
            .unwrap();
        jar.finish().unwrap();
    }

    #[test]
    fn direct_jar_replacement_only_writes_files_in_instance_mods() {
        let temporary = TemporaryDirectory::create();
        let instance = temporary.0.join("instance");
        let inside_jar = instance.join("mods/inside.jar");
        let outside_jar = temporary.0.join("Downloads/outside.jar");
        write_in_place_fixture_jar(&inside_jar, "inside");
        write_in_place_fixture_jar(&outside_jar, "outside");
        let paths = AppPaths::create(temporary.0.join("app-data")).unwrap();
        let sources = [inside_jar.clone(), outside_jar.clone()];
        let created = create_project_from_sources_core(
            &paths,
            &sources,
            Some(&instance),
            false,
            &CancellationToken::default(),
            |_, _, _, _| {},
        )
        .unwrap();
        let units = created.opened.repository().list_mod_units().unwrap();
        let inside_unit_id = units
            .iter()
            .find(|unit| unit.source_filename == "inside.jar")
            .unwrap()
            .id;
        let outside_unit_id = units
            .iter()
            .find(|unit| unit.source_filename == "outside.jar")
            .unwrap()
            .id;
        let analyses = created.analyses;
        let opened = created.opened;
        for unit in &units {
            let mut settings = analyses[&unit.id].default_review_settings();
            settings.source_locale = "en_us".to_owned();
            opened
                .writer()
                .save_review_settings(unit.id, settings, true)
                .unwrap();
            let entry = opened
                .repository()
                .list_entries(Some(unit.id))
                .unwrap()
                .into_iter()
                .next()
                .unwrap();
            opened
                .writer()
                .update_entry(EntryUpdate {
                    id: entry.id,
                    target_text: "翻訳".to_owned(),
                    origin: UpdateOrigin::Manual,
                })
                .unwrap();
        }

        let inside_before = fs::read(&inside_jar).unwrap();
        let outside_before = fs::read(&outside_jar).unwrap();
        let results = benchmark_export_selected_modified_jars(
            &opened,
            &[inside_unit_id, outside_unit_id],
            &temporary.0.join("output"),
            true,
        )
        .unwrap();

        assert_ne!(fs::read(&inside_jar).unwrap(), inside_before);
        assert_eq!(fs::read(&outside_jar).unwrap(), outside_before);
        let inside_result = results
            .iter()
            .find(|result| result.mod_unit_id == inside_unit_id)
            .unwrap();
        assert_eq!(inside_result.status, "success");
        let outside_result = results
            .iter()
            .find(|result| result.mod_unit_id == outside_unit_id)
            .unwrap();
        assert_eq!(outside_result.status, "skipped");
        assert_eq!(
            outside_result.message.as_deref(),
            Some("インスタンスの mods フォルダ外のため直接置き換えできません。")
        );

        let nested = instance.join("mods/nested");
        fs::create_dir_all(&nested).unwrap();
        assert!(original_jar_is_in_instance_mods(
            &instance,
            &nested.join("../inside.jar")
        ));
        #[cfg(windows)]
        {
            let uppercase_path = PathBuf::from(format!(
                "{}\\MODS\\INSIDE.JAR",
                instance.to_string_lossy().to_uppercase()
            ));
            assert!(original_jar_is_in_instance_mods(&instance, &uppercase_path));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let linked_jar = instance.join("mods/external-link.jar");
            symlink(&outside_jar, &linked_jar).unwrap();
            assert!(!original_jar_is_in_instance_mods(&instance, &linked_jar));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::symlink_file;

            let linked_jar = instance.join("mods/external-link.jar");
            if symlink_file(&outside_jar, &linked_jar).is_ok() {
                assert!(!original_jar_is_in_instance_mods(&instance, &linked_jar));
            }
        }
    }

    #[test]
    fn duplicate_warnings_exclude_other_locales() {
        let (_temporary, opened, mod_unit_id) = export_project_fixture();
        let source = opened
            .repository()
            .mod_unit_source_path(mod_unit_id)
            .unwrap();
        let file = fs::File::create(source).unwrap();
        let mut jar = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        for path in [
            "assets/fixture/lang/en_us.json",
            "assets/fixture/lang/ja_jp.json",
            "assets/fixture/lang/de_de.json",
        ] {
            jar.start_file(path, options).unwrap();
            jar.write_all(br#"{"key":"first","key":"last"}"#).unwrap();
        }
        jar.finish().unwrap();

        let context = export_context(&opened, mod_unit_id).unwrap();
        let paths: Vec<_> = context
            .duplicate_key_warnings
            .iter()
            .map(|(path, _)| path.as_str())
            .collect();
        assert!(paths.iter().any(|path| path.ends_with("en_us.json")));
        assert!(paths.iter().any(|path| path.ends_with("ja_jp.json")));
        assert!(!paths.iter().any(|path| path.ends_with("de_de.json")));
    }

    fn jar_replacement_fixture(opened: &OpenedProject, mod_unit_id: i64) -> JarReplacementRecord {
        let unit = opened.repository().list_mod_units().unwrap().remove(0);
        let backup = opened.directory().join("backups/original.jar");
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        fs::copy(&unit.original_path, &backup).unwrap();
        fs::write(&unit.original_path, b"translated jar").unwrap();
        opened
            .writer()
            .save_jar_replacement(JarReplacementRecord {
                id: 0,
                mod_unit_id,
                target_path: unit.original_path.clone(),
                backup_path: backup.to_string_lossy().into_owned(),
                previous_sha256: sha256_file_hex(&backup).unwrap(),
                written_sha256: sha256_file_hex(std::path::Path::new(&unit.original_path)).unwrap(),
                created_at: "2026-10-07T00:00:00Z".to_owned(),
                restored_at: None,
            })
            .unwrap();
        opened
            .repository()
            .list_jar_replacements(false)
            .unwrap()
            .remove(0)
    }

    #[test]
    fn restore_missing_jar_uses_original_backup_and_marks_record_restored() {
        let (_temporary, opened, mod_unit_id) = export_project_fixture();
        let replacement = jar_replacement_fixture(&opened, mod_unit_id);
        let original = fs::read(&replacement.backup_path).unwrap();
        fs::remove_file(&replacement.target_path).unwrap();
        let exported_paths = Arc::new(Mutex::new(std::collections::HashSet::new()));

        assert!(matches!(
            restore_modified_jar_core(&opened, replacement.id, false, &exported_paths).unwrap(),
            RestoreJarOutcome::Restored { target_path } if target_path == replacement.target_path
        ));
        assert_eq!(fs::read(&replacement.target_path).unwrap(), original);
        assert_eq!(fs::read(&replacement.backup_path).unwrap(), original);
        assert!(
            opened
                .repository()
                .list_jar_replacements(false)
                .unwrap()
                .is_empty()
        );
        assert!(
            opened.repository().list_jar_replacements(true).unwrap()[0]
                .restored_at
                .is_some()
        );
        assert!(
            exported_paths
                .lock()
                .unwrap()
                .contains(&fs::canonicalize(&replacement.target_path).unwrap())
        );
    }

    #[test]
    fn restore_changed_jar_still_requires_confirmation() {
        let (_temporary, opened, mod_unit_id) = export_project_fixture();
        let replacement = jar_replacement_fixture(&opened, mod_unit_id);
        fs::write(&replacement.target_path, b"externally changed jar").unwrap();
        let exported_paths = Arc::new(Mutex::new(std::collections::HashSet::new()));

        assert!(matches!(
            restore_modified_jar_core(&opened, replacement.id, false, &exported_paths).unwrap(),
            RestoreJarOutcome::ConfirmationRequired { .. }
        ));
        assert_eq!(
            fs::read(&replacement.target_path).unwrap(),
            b"externally changed jar"
        );
        assert_eq!(
            opened
                .repository()
                .list_jar_replacements(false)
                .unwrap()
                .len(),
            1
        );
        assert!(matches!(
            restore_modified_jar_core(&opened, replacement.id, true, &exported_paths).unwrap(),
            RestoreJarOutcome::Restored { .. }
        ));
        assert_eq!(
            fs::read(&replacement.target_path).unwrap(),
            fs::read(&replacement.backup_path).unwrap()
        );
    }

    #[test]
    fn restore_unreadable_target_preserves_backup_and_active_record() {
        let (_temporary, opened, mod_unit_id) = export_project_fixture();
        let replacement = jar_replacement_fixture(&opened, mod_unit_id);
        let original = fs::read(&replacement.backup_path).unwrap();
        fs::remove_file(&replacement.target_path).unwrap();
        fs::create_dir(&replacement.target_path).unwrap();
        let exported_paths = Arc::new(Mutex::new(std::collections::HashSet::new()));

        assert!(restore_modified_jar_core(&opened, replacement.id, true, &exported_paths).is_err());
        assert!(std::path::Path::new(&replacement.target_path).is_dir());
        assert_eq!(fs::read(&replacement.backup_path).unwrap(), original);
        assert_eq!(
            opened
                .repository()
                .list_jar_replacements(false)
                .unwrap()
                .len(),
            1
        );
        assert!(exported_paths.lock().unwrap().is_empty());
    }

    #[test]
    fn merged_direct_install_reuses_planned_name_and_requires_overwrite() {
        let (temporary, opened, mod_unit_id) = export_project_fixture_with_instance(true);
        let exported_paths = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let export = |overwrite| {
            export_selected_merged_resource_pack_core(
                &opened,
                MergedPackExportRequest {
                    selected_ids: &[mod_unit_id],
                    requested_output: None,
                    minecraft_version: "1.20.4",
                    manual_format: None,
                    direct_install: true,
                    overwrite,
                },
                &exported_paths,
            )
            .unwrap()
        };
        let MergedPackExportOutcome::Completed {
            output_path,
            directly_installed,
            ..
        } = export(false)
        else {
            panic!("first install should complete");
        };
        assert!(directly_installed);
        let output = PathBuf::from(&output_path);
        assert_eq!(
            output.parent().unwrap(),
            temporary.0.join("instance/resourcepacks")
        );
        assert!(
            !output
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".yakucraft-export-")
        );
        assert!(output.is_file());
        assert!(
            matches!(export(false), MergedPackExportOutcome::OverwriteRequired { output_path: path } if path == output_path)
        );
        fs::write(&output, b"old pack").unwrap();
        assert!(matches!(
            export(false),
            MergedPackExportOutcome::OverwriteRequired { .. }
        ));
        assert_eq!(fs::read(&output).unwrap(), b"old pack");
        assert!(
            matches!(export(true), MergedPackExportOutcome::Completed { output_path: path, .. } if path == output_path)
        );
        assert_ne!(fs::read(&output).unwrap(), b"old pack");
        assert_eq!(fs::read_dir(output.parent().unwrap()).unwrap().count(), 1);
        assert!(
            exported_paths
                .lock()
                .unwrap()
                .contains(&fs::canonicalize(&output).unwrap())
        );
        assert!(!fs::read_dir(opened.directory()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".yakucraft-export-")
        }));
    }

    #[test]
    fn export_command_rejects_a_different_active_project() {
        let (_temporary, opened, _) = export_project_fixture();
        let project_id = opened.summary().id.clone();
        let active = Some(opened);

        assert!(active_project_for_id(&active, &project_id).is_ok());
        let error = match active_project_for_id(&active, "different-project") {
            Ok(_) => panic!("a stale export must not run against the newly active project"),
            Err(error) => error,
        };
        assert_eq!(error.code, "project_changed");
    }

    #[test]
    fn selected_language_export_creates_a_missing_mod_directory() {
        let (temporary, opened, mod_unit_id) = export_project_fixture();
        let output_directory = temporary.0.join("language-output");
        let exported_paths = Arc::new(Mutex::new(std::collections::HashSet::new()));

        let results = export_selected_language_files_core(
            &opened,
            &[mod_unit_id],
            &output_directory,
            "fixture",
            &exported_paths,
        )
        .expect("selected language export should complete");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "success", "{:?}", results[0].message);
        let output = results[0]
            .output_path
            .as_deref()
            .expect("successful export should report its path");
        assert!(PathBuf::from(output).is_file());
        assert!(PathBuf::from(output).starts_with(&output_directory));
    }

    #[test]
    fn selected_kubejs_language_export_accepts_only_its_namespace_and_excludes_ftb_snbt() {
        let temporary = TemporaryDirectory::create();
        let instance = temporary.0.join("instance");
        for (relative_path, contents) in [
            (
                "minecraft/config/ftbquests/quests/lang/en_us.snbt",
                "{ quest.q.title: \"English Quest\" }",
            ),
            (
                "minecraft/kubejs/assets/example/lang/en_us.json",
                r#"{"example.key":"English KubeJS"}"#,
            ),
        ] {
            let path = instance.join(relative_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        let paths = AppPaths::create(temporary.0.join("app-data")).unwrap();
        let created = create_project_from_sources_core(
            &paths,
            &[],
            Some(&instance),
            true,
            &CancellationToken::default(),
            |_, _, _, _| {},
        )
        .expect("KubeJS and FTB language files should be imported as a Modpack unit");
        let opened = created.opened;
        let unit = opened
            .repository()
            .list_mod_units()
            .unwrap()
            .into_iter()
            .next()
            .expect("project should contain the imported Modpack unit");
        let kubejs_entry = opened
            .repository()
            .list_entries(Some(unit.id))
            .unwrap()
            .into_iter()
            .find(|entry| entry.namespace == "example")
            .expect("KubeJS language entry should be imported");
        opened
            .writer()
            .update_entry(EntryUpdate {
                id: kubejs_entry.id,
                target_text: "日本語のKubeJS".to_owned(),
                origin: UpdateOrigin::Manual,
            })
            .unwrap();

        let selected_ids = [unit.id];
        let kubejs_validation = validate_project_core_for_units(
            &opened,
            ExportKindInput::LanguageFile,
            None,
            Some("example"),
            None,
            Some(&selected_ids),
        )
        .expect("selected KubeJS namespace should validate");
        assert!(
            !kubejs_validation
                .issues
                .iter()
                .any(|issue| issue.code == "invalidExportSelection")
        );
        let ftb_validation = validate_project_core_for_units(
            &opened,
            ExportKindInput::LanguageFile,
            None,
            Some("ftbquests"),
            None,
            Some(&selected_ids),
        )
        .expect("an FTB document namespace should be reported as an invalid language selection");
        assert!(
            ftb_validation
                .issues
                .iter()
                .any(|issue| issue.code == "invalidExportSelection")
        );

        let output_directory = temporary.0.join("language-output");
        let results = export_selected_language_files_core(
            &opened,
            &selected_ids,
            &output_directory,
            "example",
            &Arc::new(Mutex::new(std::collections::HashSet::new())),
        )
        .expect("selected KubeJS language file should export");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "success", "{:?}", results[0].message);
        let output = results[0]
            .output_path
            .as_deref()
            .expect("successful KubeJS export should report its output");
        let exported: serde_json::Value =
            serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
        assert_eq!(exported["example.key"], "日本語のKubeJS");
        assert_ne!(
            PathBuf::from(output)
                .extension()
                .and_then(|value| value.to_str()),
            Some("snbt")
        );
    }

    #[test]
    fn single_language_export_records_and_returns_the_written_extension() {
        let (temporary, opened, _) = export_project_fixture();
        let requested_output = temporary.0.join("fixture-ja.lang");
        let exported_paths = Arc::new(Mutex::new(std::collections::HashSet::new()));

        let output =
            export_language_file_and_record(&opened, &requested_output, "fixture", &exported_paths)
                .expect("export should record the actual output path");

        let actual_output = temporary.0.join("fixture-ja.json");
        assert_eq!(output, actual_output);
        assert!(actual_output.is_file());
        assert!(!requested_output.exists());
        assert!(
            exported_paths
                .lock()
                .expect("exported paths should be readable")
                .contains(&fs::canonicalize(&actual_output).expect("output should canonicalize"))
        );
    }

    #[test]
    fn selected_language_export_ignores_unreadable_excluded_legacy_namespace() {
        let temporary = TemporaryDirectory::create();
        let source = temporary.0.join("fixture.jar");
        let mut jar = ZipWriter::new(fs::File::create(&source).unwrap());
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        jar.start_file("assets/fixture/lang/en_us.lang", options)
            .unwrap();
        jar.write_all(b"#PARSE_ESCAPES\nfixture.message=Source text\n")
            .unwrap();
        jar.start_file("assets/excluded/lang/en_us.lang", options)
            .unwrap();
        // 実際の解析とExportで同じ読込制限に達する、高圧縮率の言語ファイルを用意する。
        jar.write_all(&vec![b'x'; 1024 * 1024]).unwrap();
        jar.finish().unwrap();

        let paths = AppPaths::create(temporary.0.join("app-data")).unwrap();
        let created = create_project_from_sources_core(
            &paths,
            std::slice::from_ref(&source),
            None,
            false,
            &CancellationToken::default(),
            |_, _, _, _| {},
        )
        .unwrap();
        let opened = created.opened;
        let mod_unit_id = opened.repository().list_mod_units().unwrap()[0].id;
        let analysis = &created.analyses[&mod_unit_id];
        assert!(analysis.result.warnings.iter().any(|warning| {
            warning.path == "assets/excluded/lang/en_us.lang"
                && warning
                    .message
                    .starts_with("言語ファイルを読み取れませんでした")
        }));
        let settings = analysis.default_review_settings();
        assert_eq!(settings.included_namespaces, ["fixture"]);
        opened
            .writer()
            .save_review_settings(mod_unit_id, settings, true)
            .unwrap();
        let entry = opened
            .repository()
            .list_entries(Some(mod_unit_id))
            .unwrap()
            .remove(0);
        opened
            .writer()
            .update_entry(EntryUpdate {
                id: entry.id,
                target_text: "翻訳".to_owned(),
                origin: UpdateOrigin::Manual,
            })
            .unwrap();

        let exported_paths = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let results = export_selected_language_files_core(
            &opened,
            &[mod_unit_id],
            &temporary.0.join("language-output"),
            "fixture",
            &exported_paths,
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "success", "{:?}", results[0].message);
        assert_eq!(
            fs::read(results[0].output_path.as_ref().unwrap()).unwrap(),
            "#PARSE_ESCAPES\nfixture.message=翻訳\n".as_bytes()
        );

        let files =
            yc_language::discover_language_files(&[yc_language::LanguageArchivePath::root(
                "assets/excluded/lang/en_us.lang",
            )]);
        assert!(
            legacy_parse_escapes_namespaces(&source, &files, "en_us", &["excluded".to_owned()])
                .is_err()
        );
    }

    #[test]
    fn configured_source_lang_keeps_forge_parse_escapes_mode_over_japanese_file() {
        let temporary = TemporaryDirectory::create();
        let source = temporary.0.join("fixture.jar");
        let file = fs::File::create(&source).expect("fixture JAR should be created");
        let mut jar = ZipWriter::new(file);
        jar.start_file(
            "assets/fixture/lang/ja_jp.lang",
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .expect("fixture language entry should start");
        jar.write_all(b"fixture.message=Japanese text\n")
            .expect("Japanese fixture language entry should be written");
        jar.start_file(
            "assets/fixture/lang/en_us.lang",
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .expect("configured source language entry should start");
        jar.write_all(b"#PARSE_ESCAPES\nfixture.message=Source text\n")
            .expect("configured source language entry should be written");
        jar.finish().expect("fixture JAR should be finalized");

        let files = yc_language::discover_language_files(&[
            yc_language::LanguageArchivePath::root("assets/fixture/lang/ja_jp.lang"),
            yc_language::LanguageArchivePath::root("assets/fixture/lang/en_us.lang"),
        ]);
        let modes =
            legacy_parse_escapes_namespaces(&source, &files, "en_us", &["fixture".to_owned()])
                .expect("Forge escape mode should be inspected");

        assert_eq!(modes, ["fixture"]);
    }
}
