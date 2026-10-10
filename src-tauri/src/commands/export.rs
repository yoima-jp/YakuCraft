use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use tauri::State;
use yc_analysis::CancellationToken;
use yc_export::{
    ExportError, ExportInput, ExportKind, ExportValidationRequest, ManualPackFormat, MergedPackMod,
    MergedResourcePackConflict, MergedResourcePackOptions, NamespaceExport, PlaceholderWarning,
    ResourcePackMod, ResourcePackOptions, ValidationIssueCode, ValidationSeverity,
    export_merged_resource_pack, export_modified_jar as write_modified_jar,
    export_resource_pack as write_resource_pack, install_resource_pack_to,
    merged_resource_pack_display_name, merged_resource_pack_filename, replace_jar_in_place,
    replace_jar_in_place_with_backup, restore_jar_from_backup, validate_export as check_export,
};
use yc_language::{EntryFlag, ReviewSettings, TranslationEntry, WriterOptions};
use yc_project::{JarReplacementRecord, ModUnitRecord, OpenedProject, ProjectEntry};
use yc_validation::{PlaceholderSeverity, compare_placeholders};

use crate::{
    commands::project::{
        counts_for_entries, detect_minecraft_version_for_project, discovered_languages,
        merge_for_review, project_review_settings_for_unit, review_settings_from_record,
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
    pub source_filename: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub source_sha256: String,
    pub original_path: String,
    pub original_exists: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOverview {
    pub instance_path: Option<String>,
    pub units: Vec<ExportUnitStatus>,
    pub active_replacements: Vec<JarReplacementRecord>,
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
    mod_name: String,
    source_jar: PathBuf,
    source_jar_filename: String,
    mods: Vec<ResourcePackMod>,
    entries: Vec<TranslationEntry>,
    namespaces: Vec<NamespaceExport>,
    language_files: Vec<yc_language::DiscoveredLanguageFile>,
    legacy_parse_escapes_namespaces: Vec<String>,
    placeholder_warnings: Vec<PlaceholderWarning>,
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
            writer_options: WriterOptions {
                include_empty_translations: false,
                ..WriterOptions::default()
            },
            malformed_translation_files: &self.malformed_translation_files,
            placeholder_warnings: &self.placeholder_warnings,
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
        let units = opened
            .repository()
            .list_mod_units()
            .map_err(|error| project_error(error.to_string()))?
            .into_iter()
            .map(|unit| ExportUnitStatus {
                id: unit.id,
                source_filename: unit.source_filename,
                name: unit.name,
                version: unit.version,
                source_sha256: unit.source_sha256,
                original_exists: Path::new(&unit.original_path).is_file(),
                original_path: unit.original_path,
            })
            .collect();
        let active_replacements = opened
            .repository()
            .list_jar_replacements(false)
            .map_err(|error| project_error(error.to_string()))?;
        Ok(ExportOverview {
            instance_path: opened.summary().instance_path.clone(),
            units,
            active_replacements,
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
pub fn open_output_folder(path: String, state: State<'_, AppState>) -> Result<(), AppError> {
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
    #[cfg(target_os = "windows")]
    Command::new("explorer.exe")
        .arg(folder)
        .spawn()
        .map_err(export_error)?;
    #[cfg(not(target_os = "windows"))]
    return Err(AppError::simple(
        "unsupported_platform",
        "保存先フォルダーを開けません。",
    ));
    #[cfg(target_os = "windows")]
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
        ensure_output_is_safe(&context.source_jar, &unit_output)?;
        ensure_file_extension(&unit_output, "jar")?;
        ensure_has_included_namespaces(&context)?;
        write_modified_jar(&context.input(), &context.source_jar, &unit_output, true)
            .map_err(export_error)?;
        first_output.get_or_insert(unit_output);
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
    for unit in units.into_iter().filter(|unit| {
        selected_ids
            .as_ref()
            .is_none_or(|ids| ids.contains(&unit.id))
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
                    source_text: None,
                    target_text: None,
                });
                continue;
            }
        };
        let has_selection = match kind {
            ExportKindInput::LanguageFile => context.namespaces.iter().any(|candidate| {
                candidate.included && Some(candidate.namespace.as_str()) == namespace
            }),
            ExportKindInput::ResourcePack | ExportKindInput::ModifiedJar => context
                .namespaces
                .iter()
                .any(|candidate| candidate.included),
        };
        if !has_selection {
            continue;
        }
        matched_selection = true;
        let result =
            validate_export_context(&context, kind, minecraft_version, namespace, manual_format)?;
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
            message: if selected_mod_unit_ids.is_some() {
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
    let units = selected_units(opened, selected_ids)?;
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
        let Some(artifact) = artifact else {
            results.push(mod_export_result(
                unit.id,
                label,
                "skipped",
                None,
                "言語ファイルを書き出せませんでした。".to_owned(),
            ));
            continue;
        };
        let stem = Path::new(&unit.source_filename)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("mod");
        let unit_directory =
            output_directory.join(format!("{}-{}", safe_file_component(stem), unit.id));
        let output = unit_directory.join(artifact.file_name);
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
            // canonicalizeには親フォルダが必要なため、先に選択先を検証してModフォルダを作成し、
            // 作成後にシンボリックリンクも解決した実際の出力パスをもう一度検証する。
            fs::canonicalize(output_directory).map_err(export_error)?;
            fs::create_dir_all(&unit_directory).map_err(export_error)?;
            ensure_output_is_safe(&context.source_jar, &output)?;
            ensure_output_is_within_directory(output_directory, &output)?;
            atomic_write_selected_if_absent(&output, &artifact.bytes).map_err(export_error)?;
            record_exported_path(exported_paths, &output)?;
            Ok::<(), AppError>(())
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
    }
    Ok(results)
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
    let kind = export_kind(kind);
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
        let entry = namespace
            .as_deref()
            .zip(key.as_deref())
            .and_then(|(namespace, key)| {
                context
                    .entries
                    .iter()
                    .find(|entry| entry.namespace == namespace && entry.key == key)
            });
        let placeholder_comparison = (issue.code == ValidationIssueCode::PlaceholderWarning)
            .then(|| {
                let entry = entry?;
                let source = entry.source_text.as_deref()?;
                Some(compare_placeholders(source, &entry.target_text))
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
            source_text: entry.and_then(|entry| entry.source_text.clone()),
            target_text: entry.map(|entry| entry.target_text.clone()),
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
            source_text: None,
            target_text: None,
        });
    }
    let entries: Vec<_> = context
        .entries
        .iter()
        .map(|entry| ProjectEntry {
            id: 0,
            mod_unit_id: context.mod_unit_id,
            namespace: entry.namespace.clone(),
            key: entry.key.clone(),
            source_text: entry.source_text.clone(),
            source_locale: entry.source_locale.clone(),
            target_text: (!entry.target_text.is_empty()).then(|| entry.target_text.clone()),
            target_original_text: entry.target_original_text.clone(),
            translated_source_text: entry.translated_source_text.clone(),
            origin: entry.origin.clone(),
            sort_index: entry.sort_index,
            flags: entry.flags.clone(),
        })
        .collect();
    Ok(ExportValidationResult {
        issues,
        counts: counts_for_entries(&entries),
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
        ensure_output_is_safe(&context.source_jar, output_path)?;
        let input = ExportInput {
            namespaces: &selected_namespaces,
            ..context.input()
        };
        let artifacts = yc_export::create_language_files(&input).map_err(export_error)?;
        let artifact = artifacts.first().ok_or_else(|| {
            AppError::simple(
                "export",
                "言語ファイルを書き出せませんでした。Namespaceを確認してください。",
            )
        })?;
        let extension = artifact
            .file_name
            .rsplit_once('.')
            .map(|(_, extension)| extension)
            .unwrap_or_default();
        let artifact_output = output_path.with_extension(extension);
        let unit_output = output_path_for_mod(&artifact_output, unit.id, units.len());
        ensure_file_extension(&unit_output, extension)?;
        atomic_write_selected(&unit_output, &artifact.bytes).map_err(export_error)?;
        first_output.get_or_insert(unit_output);
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
    let analysis = yc_analysis::analyze_jar(&source_jar, &CancellationToken::default(), |_| {})
        .map_err(|error| {
            AppError::new(
                "archive_read",
                "JARを読み込めませんでした。プロジェクトを開き直してください。",
                Some(error.to_string()),
            )
        })?;
    let saved_settings = project_review_settings_for_unit(opened, mod_unit_id)?;
    let settings: ReviewSettings =
        review_settings_from_record(&saved_settings, &analysis, mod_unit_id);
    let mut entries = merge_for_review(&analysis, &settings).entries;
    let saved_entries = repository
        .list_entries(Some(mod_unit_id))
        .map_err(|error| project_error(error.to_string()))?;
    overlay_saved_entries(&mut entries, saved_entries);
    let placeholder_warnings = entries
        .iter_mut()
        .filter_map(|entry| {
            let mismatch = !entry.target_text.is_empty()
                && entry.source_text.as_deref().is_some_and(|source| {
                    !compare_placeholders(source, &entry.target_text).is_valid
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
    let duplicate_key_warnings = analysis
        .result
        .warnings
        .iter()
        .filter(|warning| warning.message.starts_with("重複キー"))
        .map(|warning| (warning.path.clone(), warning.message.clone()))
        .collect();
    let metadata_ranges: Vec<_> = analysis
        .result
        .mods
        .iter()
        .filter_map(|mod_info| mod_info.minecraft_version.as_deref())
        .collect();
    let minecraft_version_detection =
        detect_minecraft_version_for_project(&source_jar, &metadata_ranges);
    let language_files = discovered_languages(&analysis);
    let legacy_parse_escapes_namespaces = legacy_parse_escapes_namespaces(
        &source_jar,
        &language_files,
        &source_locale,
        &settings.included_namespaces,
    )?;
    Ok(ExportContext {
        mod_unit_id,
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
        namespaces: analysis
            .result
            .namespaces
            .iter()
            .map(|namespace| NamespaceExport {
                namespace: namespace.namespace.clone(),
                included: settings.included_namespaces.contains(&namespace.namespace),
            })
            .collect(),
        language_files,
        legacy_parse_escapes_namespaces,
        placeholder_warnings,
        malformed_translation_files,
        duplicate_key_warnings,
        source_locale,
        is_signed: analysis.result.is_signed,
        minecraft_version_detection,
    })
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
            entry.flags = saved.flags;
        }
    }
}

fn export_kind(kind: ExportKindInput) -> ExportKind {
    match kind {
        ExportKindInput::LanguageFile => ExportKind::LanguageFile,
        ExportKindInput::ResourcePack => ExportKind::ResourcePack,
        ExportKindInput::ModifiedJar => ExportKind::ModifiedJar,
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
mod export_path_regression_tests {
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        MergedPackExportOutcome, MergedPackExportRequest, RestoreJarOutcome, active_project_for_id,
        export_language_file_and_record, export_selected_language_files_core,
        export_selected_merged_resource_pack_core, legacy_parse_escapes_namespaces,
        restore_modified_jar_core, sha256_file_hex,
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
