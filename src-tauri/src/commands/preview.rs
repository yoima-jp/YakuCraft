use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Seek},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::{State, ipc::Response};
use yc_archive::{Archive, ArchiveError, ArchiveReadError, EntryKind};
use yc_assets::{
    AssetPack, AssetResolver, AssetSource, PreviewManifest, PreviewOptions, SourceKind,
};
use yc_metadata::{ArchiveLookup, find_mod_icon};
use yc_project::OpenedProject;

use crate::commands::project::detect_minecraft_version_for_project;
use crate::{error::AppError, state::AppState};

type ModIconCache = HashMap<(String, i64), Option<Vec<u8>>>;

static MOD_ICON_CACHE: OnceLock<Mutex<ModIconCache>> = OnceLock::new();

pub(crate) fn clear_mod_icon_cache() -> Result<(), AppError> {
    let Some(cache) = MOD_ICON_CACHE.get() else {
        return Ok(());
    };
    cache
        .lock()
        .map_err(|_| {
            preview_error(
                "preview_state",
                "Modアイコンキャッシュを破棄できませんでした。",
            )
        })?
        .clear();
    Ok(())
}

struct ModIconArchive(Mutex<Archive<File>>);

impl ArchiveLookup for ModIconArchive {
    fn paths(&self) -> Vec<String> {
        let Ok(archive) = self.0.lock() else {
            return Vec::new();
        };
        archive
            .list_entries()
            .iter()
            .filter(|entry| !entry.is_directory && entry.exclusion_reasons.is_empty())
            .filter_map(|entry| entry.normalized_path.as_deref().map(str::to_owned))
            .collect()
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        // Archive::read_entryはmutableなので、ArchiveLookupの共有参照からはMutex越しに読む。
        let mut archive = self.0.lock().ok()?;
        let kind = if path
            .rsplit('.')
            .next()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
        {
            // yc-metadataのアイコン上限と合わせ、不要な展開でメモリを使わない。
            EntryKind::Custom(1024 * 1024)
        } else {
            EntryKind::Text
        };
        archive.read_entry(path, kind).ok()
    }
}

/// Archive readerをProject open中に維持し、entry移動のたびにJARを開き直さない。
pub(crate) struct PreviewContext {
    pub(crate) project_id: String,
    mod_unit_id: i64,
    pub(crate) resolver: AssetResolver,
    mod_packs: Vec<AssetPack>,
    readers: HashMap<String, Arc<dyn AssetSource>>,
    allowed_asset_paths: HashSet<(String, String)>,
    minecraft_version: Option<String>,
}

impl PreviewContext {
    pub(crate) fn belongs_to_unit(&self, project_id: &str, mod_unit_id: i64) -> bool {
        self.project_id == project_id && self.mod_unit_id == mod_unit_id
    }
}

struct ArchiveAssetSource<R> {
    archive: Mutex<Archive<R>>,
    paths: Vec<String>,
}

impl<R> ArchiveAssetSource<R>
where
    R: Read + Seek + Send,
{
    fn new(archive: Archive<R>) -> Self {
        let paths = archive
            .list_entries()
            .iter()
            .filter(|entry| !entry.is_directory && entry.exclusion_reasons.is_empty())
            .filter_map(|entry| entry.normalized_path.as_deref().map(str::to_owned))
            .collect();
        Self {
            archive: Mutex::new(archive),
            paths,
        }
    }
}

impl<R> AssetSource for ArchiveAssetSource<R>
where
    R: Read + Seek + Send + 'static,
{
    fn paths(&self) -> Vec<String> {
        self.paths.clone()
    }

    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        let mut archive = self
            .archive
            .lock()
            .map_err(|_| "archive reader lock was poisoned".to_owned())?;
        let kind = if path.ends_with(".png") {
            EntryKind::Texture
        } else {
            EntryKind::ModelJson
        };
        match archive.read_entry_independent(path, kind) {
            Ok(bytes) => Ok(Some(bytes)),
            // 静止textureには副ファイルがないのが通常。破損やサイズ制限のエラーは隠さない。
            Err(ArchiveError::ArchiveRead(ArchiveReadError::EntryNotFound { .. }))
                if path.ends_with(".png.mcmeta") =>
            {
                Ok(None)
            }
            Err(_) => Err(format!("could not read archive path {path}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MinecraftJarCandidate {
    pub version: String,
    pub path: String,
}

#[tauri::command]
pub async fn get_asset_preview_manifest(
    state: State<'_, AppState>,
    entry_id: i64,
    variant: Option<PreviewOptions>,
) -> Result<PreviewManifest, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let project_io_lock = Arc::clone(&state.project_io_lock);
    let context_cache = state.preview_context.clone();
    let paths = state.paths.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _project_io = project_io_lock
            .lock()
            .map_err(|_| preview_error("preview_state", "プレビューを読み込めませんでした。"))?;
        let (project_id, project_directory) = active_project_directory(&active_project)?;
        let repository = yc_project::Repository::new(project_directory.join("project.sqlite"));
        let entry = repository
            .list_entries(None)
            .map_err(|_| preview_error("preview_entry", "プレビューを読み込めませんでした。"))?
            .into_iter()
            .find(|entry| entry.id == entry_id)
            .ok_or_else(|| preview_error("preview_entry", "翻訳項目が見つかりません。"))?;

        let mut cached = context_cache
            .lock()
            .map_err(|_| preview_error("preview_state", "プレビューを読み込めませんでした。"))?;
        if cached.as_ref().is_none_or(|context| {
            context.project_id != project_id || context.mod_unit_id != entry.mod_unit_id
        }) {
            let source_jar = repository
                .mod_unit_source_path(entry.mod_unit_id)
                .map_err(|_| {
                    preview_error(
                        "preview_source",
                        "プロジェクトのアセットを読み込めませんでした。",
                    )
                })?;
            let minecraft_path = read_minecraft_jar_setting(&paths.settings_file)
                .ok()
                .flatten();
            *cached = Some(build_preview_context(
                &project_id,
                entry.mod_unit_id,
                &project_directory,
                &source_jar,
                minecraft_path.as_deref().map(Path::new),
            )?);
        }

        let context = cached
            .as_mut()
            .expect("preview context was just initialized");
        let manifest = context
            .resolver
            .resolve_preview_with_options(&entry.key, &variant.unwrap_or_default());
        add_manifest_paths(context, &manifest);
        Ok(manifest)
    })
    .await
    .map_err(|_| preview_error("preview_task", "プレビューを読み込めませんでした。"))?
}

#[tauri::command]
pub async fn get_asset_bytes(
    state: State<'_, AppState>,
    source: String,
    path: String,
) -> Result<Response, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let project_io_lock = Arc::clone(&state.project_io_lock);
    let context_cache = state.preview_context.clone();
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        let _project_io = project_io_lock
            .lock()
            .map_err(|_| preview_error("preview_state", "アセットを読み込めませんでした。"))?;
        let active_project_id = active_project_id(&active_project)?;
        let cached = context_cache
            .lock()
            .map_err(|_| preview_error("preview_state", "アセットを読み込めませんでした。"))?;
        let context = cached
            .as_ref()
            .filter(|context| context.project_id == active_project_id)
            .ok_or_else(|| {
                preview_error(
                    "preview_asset_not_allowed",
                    "アセットを読み込めませんでした。",
                )
            })?;
        if !is_manifest_path_allowed(&context.allowed_asset_paths, &source, &path) {
            return Err(preview_error(
                "preview_asset_not_allowed",
                "このプレビューからアセットを読み込めません。",
            ));
        }
        let reader = context.readers.get(&source).ok_or_else(|| {
            preview_error(
                "preview_asset_not_allowed",
                "アセットを読み込めませんでした。",
            )
        })?;
        read_preview_asset(reader.as_ref(), &path)
    })
    .await
    .map_err(|_| preview_error("preview_task", "アセットを読み込めませんでした。"))??;
    Ok(Response::new(bytes))
}

#[tauri::command]
pub async fn get_mod_icon(
    state: State<'_, AppState>,
    mod_unit_id: Option<i64>,
    clear_cache: Option<bool>,
) -> Result<Option<Vec<u8>>, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let project_io_lock = Arc::clone(&state.project_io_lock);
    tauri::async_runtime::spawn_blocking(move || {
        let _project_io = project_io_lock
            .lock()
            .map_err(|_| preview_error("preview_state", "Modアイコンを読み込めませんでした。"))?;
        if clear_cache.unwrap_or(false) {
            clear_mod_icon_cache()?;
            return Ok(None);
        }
        let mod_unit_id = mod_unit_id.ok_or_else(|| {
            preview_error("preview_source", "Modアイコンを読み込めませんでした。")
        })?;
        let (project_id, project_directory) = active_project_directory(&active_project)?;
        let cache = MOD_ICON_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        {
            let mut cache = cache
                .lock()
                .map_err(|_| preview_error("preview_state", "Modアイコンを読み込めませんでした。"))?;
            // プロジェクト切り替え時に前の画像bytesを解放し、同じunit IDの取り違えを防ぐ。
            cache.retain(|(cached_project_id, _), _| cached_project_id == &project_id);
            if let Some(icon) = cache.get(&(project_id.clone(), mod_unit_id)) {
                return Ok(icon.clone());
            }
        }

        let repository = yc_project::Repository::new(project_directory.join("project.sqlite"));
        let unit = repository
            .list_mod_units()
            .map_err(|error| {
                tracing::error!(%error, "Mod units could not be read for icon lookup");
                preview_error("preview_source", "Modアイコンを読み込めませんでした。")
            })?
            .into_iter()
            .find(|unit| unit.id == mod_unit_id);
        let icon = if let Some(unit) = unit {
            let source_path = repository
                .mod_unit_source_path(mod_unit_id)
                .map_err(|error| {
                    tracing::error!(%error, mod_unit_id, "Mod source path could not be read for icon lookup");
                    preview_error("preview_source", "Modアイコンを読み込めませんでした。")
                })?;
            match Archive::open(source_path) {
                Ok(archive) => find_mod_icon(&ModIconArchive(Mutex::new(archive)), unit.mod_id.as_deref())
                    .map(|icon| icon.bytes),
                Err(error) => {
                    tracing::debug!(%error, mod_unit_id, "Mod icon archive could not be opened");
                    None
                }
            }
        } else {
            None
        };

        cache
            .lock()
            .map_err(|_| preview_error("preview_state", "Modアイコンを読み込めませんでした。"))?
            .insert((project_id, mod_unit_id), icon.clone());
        Ok(icon)
    })
    .await
    .map_err(|_| preview_error("preview_task", "Modアイコンを読み込めませんでした。"))?
}

#[tauri::command]
pub async fn load_thumbnail(
    state: State<'_, AppState>,
    project_id: String,
    key: String,
) -> Result<Option<Vec<u8>>, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let project_io_lock = Arc::clone(&state.project_io_lock);
    tauri::async_runtime::spawn_blocking(move || {
        let _project_io = project_io_lock
            .lock()
            .map_err(|_| preview_error("preview_state", "サムネイルを読み込めませんでした。"))?;
        let directory = thumbnail_cache_directory(&active_project, &project_id)?;
        let path = thumbnail_path(&directory, &key);
        match fs::read(&path) {
            Ok(bytes) if valid_thumbnail_png(&bytes) => Ok(Some(bytes)),
            Ok(_) => {
                // 書き込み中断などで壊れたキャッシュは削除し、次のアイドル処理で作り直す。
                let _ = fs::remove_file(path);
                Ok(None)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(preview_error(
                "thumbnail_read",
                "サムネイルを読み込めませんでした。",
            )),
        }
    })
    .await
    .map_err(|_| preview_error("thumbnail_task", "サムネイルを読み込めませんでした。"))?
}

#[tauri::command]
pub async fn save_thumbnail(
    state: State<'_, AppState>,
    project_id: String,
    key: String,
    bytes: Vec<u8>,
) -> Result<(), AppError> {
    if !valid_thumbnail_png(&bytes) {
        return Err(preview_error(
            "thumbnail_format",
            "サムネイルを保存できませんでした。",
        ));
    }
    let active_project = Arc::clone(&state.active_project);
    let project_io_lock = Arc::clone(&state.project_io_lock);
    tauri::async_runtime::spawn_blocking(move || {
        let _project_io = project_io_lock
            .lock()
            .map_err(|_| preview_error("preview_state", "サムネイルを保存できませんでした。"))?;
        let directory = thumbnail_cache_directory(&active_project, &project_id)?;
        fs::create_dir_all(&directory)
            .map_err(|_| preview_error("thumbnail_write", "サムネイルを保存できませんでした。"))?;
        let path = thumbnail_path(&directory, &key);
        let temporary_path = directory.join(format!(
            "{}.{}.tmp",
            thumbnail_file_name(&key),
            std::process::id()
        ));
        fs::write(&temporary_path, bytes)
            .map_err(|_| preview_error("thumbnail_write", "サムネイルを保存できませんでした。"))?;
        if fs::rename(&temporary_path, &path).is_err() {
            // Windowsでは既存ファイルの置き換えrenameが失敗する場合があるため、キャッシュだけを入れ替える。
            if path.exists() {
                fs::remove_file(&path)
                    .and_then(|()| fs::rename(&temporary_path, &path))
                    .map_err(|_| {
                        preview_error("thumbnail_write", "サムネイルを保存できませんでした。")
                    })?;
            } else {
                let _ = fs::remove_file(&temporary_path);
                return Err(preview_error(
                    "thumbnail_write",
                    "サムネイルを保存できませんでした。",
                ));
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| preview_error("thumbnail_task", "サムネイルを保存できませんでした。"))?
}

fn thumbnail_cache_directory(
    active_project: &Mutex<Option<OpenedProject>>,
    project_id: &str,
) -> Result<PathBuf, AppError> {
    let (active_project_id, directory) = active_project_directory(active_project)?;
    if active_project_id != project_id {
        return Err(preview_error(
            "project_changed",
            "プロジェクトを読み込めませんでした。",
        ));
    }
    Ok(project_cache_directory(&directory).join("thumbnails"))
}

/// サムネイルなど再生成できるデータ専用の場所。DBや入力JARとは分けて削除できるようにする。
pub(crate) fn project_cache_directory(project_directory: &Path) -> PathBuf {
    project_directory.join("cache")
}

fn thumbnail_path(directory: &Path, key: &str) -> PathBuf {
    directory.join(format!("{}.png", thumbnail_file_name(key)))
}

fn thumbnail_file_name(key: &str) -> String {
    Sha256::digest(key.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn valid_thumbnail_png(bytes: &[u8]) -> bool {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    bytes.len() <= 128 * 1024 && bytes.starts_with(PNG_SIGNATURE)
}

#[tauri::command]
pub async fn detect_minecraft_jars(
    state: State<'_, AppState>,
) -> Result<Vec<MinecraftJarCandidate>, AppError> {
    let active_project = Arc::clone(&state.active_project);
    let project_io_lock = Arc::clone(&state.project_io_lock);
    let context_cache = state.preview_context.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _project_io = project_io_lock
            .lock()
            .map_err(|_| preview_error("preview_state", "Minecraft JARを検索できませんでした。"))?;
        let project = active_project_location(&active_project)?;
        let preferred_version = project.and_then(|(project_id, source_jar)| {
            context_cache
                .lock()
                .ok()
                .and_then(|cache| {
                    cache
                        .as_ref()
                        .filter(|context| context.project_id == project_id)
                        .and_then(|context| context.minecraft_version.clone())
                })
                .or_else(|| project_minecraft_version(&source_jar))
        });
        Ok(detect_minecraft_jars_from_known_locations(
            preferred_version.as_deref(),
        ))
    })
    .await
    .map_err(|_| {
        preview_error(
            "minecraft_jar_detection",
            "Minecraft JARを検索できませんでした。",
        )
    })?
}

#[tauri::command]
pub async fn set_minecraft_jar(
    state: State<'_, AppState>,
    path: Option<String>,
) -> Result<(), AppError> {
    let settings_file = state.paths.settings_file.clone();
    let project_io_lock = Arc::clone(&state.project_io_lock);
    let context_cache = state.preview_context.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _project_io = project_io_lock
            .lock()
            .map_err(|_| preview_error("preview_state", "設定を保存できませんでした。"))?;
        let selected_path = path
            .as_deref()
            .map(|path| validate_selected_minecraft_jar(Path::new(path)))
            .transpose()?;
        let selected_path_string = selected_path
            .as_deref()
            .map(|path| path.to_string_lossy().into_owned());
        super::settings::update_settings_file(
            &settings_file,
            |settings| {
                let preview = settings
                    .values
                    .entry("preview".to_owned())
                    .or_insert_with(|| Value::Object(Default::default()));
                let Some(preview) = preview.as_object_mut() else {
                    return Err(preview_error(
                        "settings_read",
                        "設定を保存できませんでした。",
                    ));
                };
                preview.insert(
                    "minecraftJarPath".to_owned(),
                    selected_path_string
                        .as_deref()
                        .map_or(Value::Null, |path| Value::String(path.to_owned())),
                );
                Ok(())
            },
            |_| preview_error("settings_read", "設定を保存できませんでした。"),
            |_| preview_error("settings_write", "設定を保存できませんでした。"),
        )?;
        let mut cached = context_cache
            .lock()
            .map_err(|_| preview_error("preview_state", "設定を保存できませんでした。"))?;
        if let Some(context) = cached.as_mut() {
            configure_minecraft_pack(context, selected_path.as_deref());
        }
        Ok(())
    })
    .await
    .map_err(|_| preview_error("preview_task", "設定を保存できませんでした。"))?
}

#[tauri::command]
pub async fn get_minecraft_jar_setting(
    state: State<'_, AppState>,
) -> Result<Option<String>, AppError> {
    let settings_file = state.paths.settings_file.clone();
    tauri::async_runtime::spawn_blocking(move || read_minecraft_jar_setting(&settings_file))
        .await
        .map_err(|_| preview_error("preview_task", "設定を読み込めませんでした。"))?
}

fn active_project_directory(
    active_project: &Mutex<Option<OpenedProject>>,
) -> Result<(String, PathBuf), AppError> {
    let active = active_project
        .lock()
        .map_err(|_| preview_error("preview_state", "プロジェクトを読み込めませんでした。"))?;
    let project = active
        .as_ref()
        .ok_or_else(|| preview_error("no_open_project", "先にプロジェクトを開いてください。"))?;
    Ok((
        project.summary().id.clone(),
        project.directory().to_path_buf(),
    ))
}

fn active_project_location(
    active_project: &Mutex<Option<OpenedProject>>,
) -> Result<Option<(String, PathBuf)>, AppError> {
    let active = active_project
        .lock()
        .map_err(|_| preview_error("preview_state", "プロジェクトを読み込めませんでした。"))?;
    active
        .as_ref()
        .map(|project| {
            let repository = project.repository();
            let first_unit = repository
                .list_mod_units()
                .map_err(|_| {
                    preview_error(
                        "preview_source",
                        "Minecraftのバージョンを判定できませんでした。",
                    )
                })?
                .into_iter()
                .next();
            let source_path = first_unit
                .map(|unit| repository.mod_unit_source_path(unit.id))
                .transpose()
                .map_err(|_| {
                    preview_error(
                        "preview_source",
                        "Minecraftのバージョンを判定できませんでした。",
                    )
                })?;
            Ok(source_path.map(|source_path| (project.summary().id.clone(), source_path)))
        })
        .transpose()
        .map(Option::flatten)
}

fn active_project_id(active_project: &Mutex<Option<OpenedProject>>) -> Result<String, AppError> {
    active_project_directory(active_project).map(|(project_id, _)| project_id)
}

fn build_preview_context(
    project_id: &str,
    mod_unit_id: i64,
    project_directory: &Path,
    source_jar: &Path,
    minecraft_jar: Option<&Path>,
) -> Result<PreviewContext, AppError> {
    let analysis = yc_analysis::analyze_jar(
        source_jar,
        &yc_analysis::CancellationToken::default(),
        |_| {},
    )
    .ok();
    let metadata_ranges: Vec<_> = analysis
        .as_ref()
        .into_iter()
        .flat_map(|analysis| {
            analysis
                .result
                .mods
                .iter()
                .filter_map(|mod_info| mod_info.minecraft_version.as_deref())
        })
        .collect();
    let minecraft_version = detect_minecraft_version_for_project(source_jar, &metadata_ranges)
        .map(|detection| detection.version);
    let root_archive = Archive::open(source_jar).map_err(|_| {
        preview_error(
            "preview_source",
            "プロジェクトのアセットを読み込めませんでした。",
        )
    })?;
    let root_source = Arc::new(ArchiveAssetSource::new(root_archive));
    let mut readers: HashMap<String, Arc<dyn AssetSource>> = HashMap::new();
    readers.insert("mod-root".to_owned(), root_source.clone());
    let mut packs = vec![AssetPack::new(
        "mod-root",
        SourceKind::Mod,
        root_source.clone(),
    )];

    if let Some(analysis) = analysis {
        let included = yc_project::Repository::new(project_directory.join("project.sqlite"))
            .review_settings()
            .map(|settings| {
                settings
                    .included_namespaces
                    .into_iter()
                    .filter(|selection| selection.mod_unit_id == mod_unit_id)
                    .map(|selection| selection.namespace)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_else(|_| {
                analysis
                    .result
                    .namespaces
                    .iter()
                    .filter(|namespace| namespace.included)
                    .map(|namespace| namespace.namespace.clone())
                    .collect()
            });
        // namespacesは名前で重複除去されるため、Rootと同namespaceのNestedは
        // 言語ファイルの提供元からも拾う。対象外namespaceのNestedは追加しない。
        let nested_paths: Vec<_> = analysis
            .result
            .namespaces
            .iter()
            .map(|namespace| (&namespace.namespace, &namespace.container_path))
            .chain(
                analysis
                    .result
                    .language_files
                    .iter()
                    .map(|file| (&file.namespace, &file.container_path)),
            )
            .filter(|(namespace, container_path)| {
                !container_path.is_empty() && included.contains(*namespace)
            })
            .map(|(_, container_path)| container_path.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if !nested_paths.is_empty() {
            let mut root = root_source.archive.lock().map_err(|_| {
                preview_error(
                    "preview_source",
                    "プロジェクトのアセットを読み込めませんでした。",
                )
            })?;
            if let Ok(nested_archives) = root.discover_nested(&nested_paths) {
                for nested in nested_archives {
                    if !nested_paths.contains(&nested.path) {
                        continue;
                    }
                    let Some(archive) = nested.archive else {
                        continue;
                    };
                    let source_id = nested_source_id(&nested.path);
                    let source = Arc::new(ArchiveAssetSource::new(archive));
                    readers.insert(source_id.clone(), source.clone());
                    packs.push(AssetPack::new(source_id, SourceKind::Mod, source));
                }
            }
        }
    }

    let mod_packs = packs.clone();

    if let Some(path) = minecraft_jar {
        // 本体JARは任意設定のため、開けない場合はBuiltinの解決結果をそのまま使う。
        if let Ok(archive) = Archive::open(path) {
            let source_id = "minecraft-jar".to_owned();
            let source = Arc::new(ArchiveAssetSource::new(archive));
            readers.insert(source_id.clone(), source.clone());
            packs.push(AssetPack::new(source_id, SourceKind::Minecraft, source));
        }
    }

    Ok(PreviewContext {
        project_id: project_id.to_owned(),
        mod_unit_id,
        resolver: AssetResolver::new(packs),
        mod_packs,
        readers,
        allowed_asset_paths: HashSet::new(),
        minecraft_version,
    })
}

fn configure_minecraft_pack(context: &mut PreviewContext, minecraft_jar: Option<&Path>) {
    context.readers.remove("minecraft-jar");
    let mut packs = context.mod_packs.clone();
    if let Some(path) = minecraft_jar
        && let Ok(archive) = Archive::open(path)
    {
        let source_id = "minecraft-jar".to_owned();
        let source = Arc::new(ArchiveAssetSource::new(archive));
        context.readers.insert(source_id.clone(), source.clone());
        packs.push(AssetPack::new(source_id, SourceKind::Minecraft, source));
    }
    context.resolver = AssetResolver::new(packs);
    context.allowed_asset_paths.clear();
}

fn nested_source_id(path: &str) -> String {
    format!("mod-nested-{}", path.replace('/', "_"))
}

fn project_minecraft_version(source_jar: &Path) -> Option<String> {
    let analysis = yc_analysis::analyze_jar(
        source_jar,
        &yc_analysis::CancellationToken::default(),
        |_| {},
    )
    .ok()?;
    let metadata_ranges: Vec<_> = analysis
        .result
        .mods
        .iter()
        .filter_map(|mod_info| mod_info.minecraft_version.as_deref())
        .collect();
    detect_minecraft_version_for_project(source_jar, &metadata_ranges)
        .map(|detection| detection.version)
}

fn add_manifest_paths(context: &mut PreviewContext, manifest: &PreviewManifest) {
    let Ok(value) = serde_json::to_value(manifest) else {
        return;
    };
    let source_ids: HashSet<_> = context.readers.keys().map(String::as_str).collect();
    collect_manifest_asset_paths(&value, &source_ids, &mut context.allowed_asset_paths);
}

fn collect_manifest_asset_paths(
    value: &Value,
    source_ids: &HashSet<&str>,
    output: &mut HashSet<(String, String)>,
) {
    match value {
        Value::Object(object) => {
            if let (Some(source), Some(path)) = (
                object.get("sourceId").and_then(Value::as_str),
                object.get("archivePath").and_then(Value::as_str),
            ) && source_ids.contains(source)
            {
                output.insert((source.to_owned(), path.to_owned()));
            }
            for child in object.values() {
                collect_manifest_asset_paths(child, source_ids, output);
            }
        }
        Value::Array(children) => {
            for child in children {
                collect_manifest_asset_paths(child, source_ids, output);
            }
        }
        _ => {}
    }
}

fn is_manifest_path_allowed(
    allowed_paths: &HashSet<(String, String)>,
    source: &str,
    path: &str,
) -> bool {
    allowed_paths.contains(&(source.to_owned(), path.to_owned()))
        || path.strip_suffix(".mcmeta").is_some_and(|texture| {
            // 副ファイルは同じsourceのmanifest掲載textureに限定し、任意のJSON読み取りを許可しない。
            texture.starts_with("assets/")
                && texture.contains("/textures/")
                && texture.ends_with(".png")
                && allowed_paths.contains(&(source.to_owned(), texture.to_owned()))
        })
}

fn read_preview_asset(reader: &dyn AssetSource, path: &str) -> Result<Vec<u8>, AppError> {
    let bytes = reader
        .read(path)
        .map_err(|_| preview_error("preview_asset_read", "アセットを読み込めませんでした。"))?;
    match bytes {
        Some(bytes) => Ok(bytes),
        None if path.ends_with(".png.mcmeta") => Ok(Vec::new()),
        None => Err(preview_error(
            "preview_asset_read",
            "アセットを読み込めませんでした。",
        )),
    }
}

fn detect_minecraft_jars_from_known_locations(
    preferred_version: Option<&str>,
) -> Vec<MinecraftJarCandidate> {
    let mut candidates = minecraft_versions_dirs()
        .iter()
        .flat_map(|versions_dir| {
            detect_minecraft_jars_in_versions_dir(versions_dir, preferred_version)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        let left_matches = version_matches(preferred_version, &left.version);
        let right_matches = version_matches(preferred_version, &right.version);
        right_matches
            .cmp(&left_matches)
            .then_with(|| left.version.cmp(&right.version))
            .then_with(|| left.path.cmp(&right.path))
    });
    candidates
}

fn minecraft_versions_dirs() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|appdata| vec![appdata.join(".minecraft").join("versions")])
            .unwrap_or_default()
    }

    #[cfg(target_os = "linux")]
    {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return Vec::new();
        };
        let mut versions_dirs = vec![home.join(".minecraft").join("versions")];
        // 公式ランチャーとPrismの標準・Flatpak保存先を探し、各インスタンスのMinecraft JARも候補にする。
        for launcher_data in [
            home.join(".local/share/PrismLauncher"),
            home.join(".var/app/org.prismlauncher.PrismLauncher/data/PrismLauncher"),
        ] {
            // PrismはクライアントJARを共有ライブラリ領域に置くため、インスタンス内だけでなくここも探す。
            versions_dirs.push(launcher_data.join("libraries/com/mojang/minecraft"));
            let instances = launcher_data.join("instances");
            let Ok(entries) = fs::read_dir(instances) else {
                continue;
            };
            versions_dirs.extend(entries.flatten().filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|file_type| file_type.is_dir())
                    .map(|_| entry.path().join(".minecraft/versions"))
            }));
        }
        versions_dirs
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    Vec::new()
}

fn detect_minecraft_jars_in_versions_dir(
    versions_dir: &Path,
    preferred_version: Option<&str>,
) -> Vec<MinecraftJarCandidate> {
    let Ok(versions) = fs::read_dir(versions_dir) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for version_dir in versions.flatten() {
        let Ok(file_type) = version_dir.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let version = version_dir.file_name().to_string_lossy().into_owned();
        let Ok(files) = fs::read_dir(version_dir.path()) else {
            continue;
        };
        for file in files.flatten() {
            if !file.file_type().is_ok_and(|file_type| file_type.is_file()) {
                continue;
            }
            let path = file.path();
            if path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
            {
                candidates.push(MinecraftJarCandidate {
                    version: version.clone(),
                    path: path.to_string_lossy().into_owned(),
                });
            }
        }
    }
    candidates.sort_by(|left, right| {
        let left_matches = version_matches(preferred_version, &left.version);
        let right_matches = version_matches(preferred_version, &right.version);
        right_matches
            .cmp(&left_matches)
            .then_with(|| left.version.cmp(&right.version))
            .then_with(|| left.path.cmp(&right.path))
    });
    candidates
}

fn version_matches(preferred: Option<&str>, candidate: &str) -> bool {
    preferred.is_some_and(|preferred| {
        candidate == preferred || candidate.starts_with(&format!("{preferred}."))
    })
}

fn validate_minecraft_jar(path: &Path) -> Result<(), AppError> {
    let archive = Archive::open(path)
        .map_err(|_| preview_error("invalid_minecraft_jar", "Minecraft JARを開けませんでした。"))?;
    if archive.list_entries().iter().any(|entry| {
        entry
            .normalized_path
            .as_deref()
            .is_some_and(|path| path.starts_with("assets/minecraft/models/"))
    }) {
        Ok(())
    } else {
        Err(preview_error(
            "invalid_minecraft_jar",
            "Minecraftのモデルを含むJARを選んでください。",
        ))
    }
}

fn validate_selected_minecraft_jar(path: &Path) -> Result<PathBuf, AppError> {
    let invalid_selection = || {
        preview_error(
            "invalid_minecraft_jar",
            "検出済みのMinecraft JARを選んでください。",
        )
    };
    let selected_path = fs::canonicalize(path).map_err(|_| invalid_selection())?;
    let is_detected_candidate = minecraft_versions_dirs()
        .into_iter()
        .filter_map(|versions_dir| fs::canonicalize(versions_dir).ok())
        .filter(|versions_root| selected_path.starts_with(versions_root))
        .any(|versions_root| {
            detect_minecraft_jars_in_versions_dir(&versions_root, None)
                .iter()
                .any(|candidate| {
                    fs::canonicalize(&candidate.path)
                        .is_ok_and(|candidate_path| candidate_path == selected_path)
                })
        });
    if !is_detected_candidate {
        return Err(invalid_selection());
    }

    validate_minecraft_jar(&selected_path)?;
    Ok(selected_path)
}

fn read_minecraft_jar_setting(path: &Path) -> Result<Option<String>, AppError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(preview_error(
                "settings_read",
                "設定を読み込めませんでした。",
            ));
        }
    };
    let settings: Value = serde_json::from_str(&contents)
        .map_err(|_| preview_error("settings_read", "設定を読み込めませんでした。"))?;
    let Some(path) = settings
        .get("preview")
        .and_then(|preview| preview.get("minecraftJarPath"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    // 保存済み設定も再検証し、移動済み・範囲外・不正なJARはBuiltinへfallbackする。
    Ok(validate_selected_minecraft_jar(Path::new(path))
        .ok()
        .map(|path| path.to_string_lossy().into_owned()))
}

fn preview_error(code: &str, message: &str) -> AppError {
    AppError::simple(code, message)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{Cursor, Write},
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    use super::{
        ArchiveAssetSource, build_preview_context, detect_minecraft_jars_in_versions_dir,
        is_manifest_path_allowed, nested_source_id, read_preview_asset, validate_minecraft_jar,
    };
    use crate::{
        commands::project::prepare_project_for_deletion, paths::AppPaths, state::AppState,
    };
    use uuid::Uuid;

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn create() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after the epoch")
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("yakucraft-preview-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&path).expect("temporary directory should be created");
            Self(path)
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_jar(path: &std::path::Path, include_models: bool) {
        fs::create_dir_all(path.parent().expect("fixture has a parent")).unwrap();
        let file = fs::File::create(path).expect("fixture JAR should be created");
        let mut jar = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        if include_models {
            jar.start_file("assets/minecraft/models/block/stone.json", options)
                .unwrap();
            jar.write_all(b"{}").unwrap();
        } else {
            jar.start_file("empty.txt", options).unwrap();
            jar.write_all(b"fixture").unwrap();
        }
        jar.finish().expect("fixture JAR should finish");
    }

    fn jar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut jar = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (path, bytes) in entries {
            jar.start_file(*path, options).unwrap();
            jar.write_all(bytes).unwrap();
        }
        jar.finish().unwrap().into_inner()
    }

    #[test]
    fn preview_loads_nested_assets_sharing_the_included_root_namespace() {
        let temporary = TemporaryDirectory::create();
        let nested_path = "META-INF/jars/shared.jar";
        let model_path = "assets/example/models/item/nested.json";
        let model = br#"{"elements":[]}"#;
        let nested = jar_bytes(&[
            (
                "assets/example/lang/en_us.json",
                br#"{"item.example.nested":"Nested"}"#,
            ),
            (model_path, model),
        ]);
        let excluded = jar_bytes(&[
            (
                "assets/excluded/lang/en_us.json",
                br#"{"item.excluded.other":"Other"}"#,
            ),
            ("assets/excluded/models/item/other.json", model),
        ]);
        let source = temporary.0.join("source.jar");
        fs::write(
            &source,
            jar_bytes(&[
                (
                    "assets/example/lang/en_us.json",
                    br#"{"item.example.root":"Root"}"#,
                ),
                (nested_path, &nested),
                ("META-INF/jars/excluded.jar", &excluded),
            ]),
        )
        .unwrap();
        let projects = temporary.0.join("projects");
        let summary =
            yc_project::create_project(&projects, std::slice::from_ref(&source), None).unwrap();
        let opened =
            yc_project::open_project(&projects, Uuid::parse_str(&summary.id).unwrap()).unwrap();
        let analysis =
            yc_analysis::analyze_jar(&source, &yc_analysis::CancellationToken::default(), |_| {})
                .unwrap();
        assert!(analysis.result.namespaces.iter().any(|namespace| {
            namespace.namespace == "example" && namespace.container_path.is_empty()
        }));
        opened
            .writer()
            .save_analysis(
                1,
                yc_project::AnalysisData {
                    mods: Vec::new(),
                    namespaces: analysis
                        .result
                        .namespaces
                        .iter()
                        .map(|namespace| yc_project::NamespaceRecord {
                            mod_unit_id: 1,
                            namespace: namespace.namespace.clone(),
                            container_path: namespace.container_path.clone(),
                            included: namespace.included,
                            entry_count: 1,
                        })
                        .collect(),
                    nested_archives: Vec::new(),
                    languages: Vec::new(),
                    entries: Vec::new(),
                },
            )
            .unwrap();
        opened
            .writer()
            .save_review_settings(
                1,
                yc_language::ReviewSettings {
                    included_namespaces: vec!["example".to_owned()],
                    ..Default::default()
                },
                true,
            )
            .unwrap();

        let context =
            build_preview_context(&summary.id, 1, opened.directory(), &source, None).unwrap();
        let reader = context
            .readers
            .get(&nested_source_id(nested_path))
            .expect("shared namespace nested reader");
        assert_eq!(reader.read(model_path).unwrap().unwrap(), model);
        assert!(
            context
                .resolver
                .resolve_model("example:item/nested")
                .is_ok()
        );
        assert!(
            !context
                .readers
                .contains_key(&nested_source_id("META-INF/jars/excluded.jar"))
        );
    }

    #[test]
    fn preview_reader_can_read_assets_repeatedly_beyond_one_analysis_budget() {
        use yc_assets::AssetSource;
        let temporary = TemporaryDirectory::create();
        let source = temporary.0.join("source.jar");
        let path = "assets/example/models/item/test.json";
        fs::write(&source, jar_bytes(&[(path, b"{}")])).unwrap();
        let limits = yc_archive::ArchiveLimits {
            max_cumulative_read_bytes: 2,
            ..Default::default()
        };
        let reader = ArchiveAssetSource::new(
            yc_archive::Archive::open_with_limits(&source, limits).unwrap(),
        );
        for _ in 0..4 {
            assert_eq!(reader.read(path).unwrap().unwrap(), b"{}");
        }
    }

    #[test]
    fn preview_reads_animation_metadata_and_treats_only_missing_sidecars_as_optional() {
        let temporary = TemporaryDirectory::create();
        let source = temporary.0.join("source.jar");
        let path = "assets/example/textures/block/animated.png.mcmeta";
        let metadata = br#"{"animation":{"frames":[2]}}"#;
        fs::write(&source, jar_bytes(&[(path, metadata)])).unwrap();
        let reader = ArchiveAssetSource::new(yc_archive::Archive::open(&source).unwrap());
        assert_eq!(read_preview_asset(&reader, path).unwrap(), metadata);
        assert!(
            read_preview_asset(&reader, "assets/example/textures/block/static.png.mcmeta")
                .unwrap()
                .is_empty()
        );
        assert!(read_preview_asset(&reader, "assets/example/textures/block/missing.png").is_err());

        let limits = yc_archive::ArchiveLimits {
            max_model_json_size_bytes: 1,
            ..Default::default()
        };
        let limited = ArchiveAssetSource::new(
            yc_archive::Archive::open_with_limits(&source, limits).unwrap(),
        );
        assert!(read_preview_asset(&limited, path).is_err());
    }

    #[test]
    fn asset_bytes_are_limited_to_paths_from_a_manifest() {
        let allowed = std::collections::HashSet::from([(
            "mod-root".to_owned(),
            "assets/example/textures/block/stone.png".to_owned(),
        )]);
        assert!(is_manifest_path_allowed(
            &allowed,
            "mod-root",
            "assets/example/textures/block/stone.png"
        ));
        assert!(!is_manifest_path_allowed(
            &allowed,
            "mod-root",
            "assets/example/textures/block/other.png"
        ));
        assert!(!is_manifest_path_allowed(
            &allowed,
            "minecraft-jar",
            "assets/example/textures/block/stone.png"
        ));
        assert!(is_manifest_path_allowed(
            &allowed,
            "mod-root",
            "assets/example/textures/block/stone.png.mcmeta"
        ));
        for (source, path) in [
            ("mod-root", "assets/example/textures/block/other.png.mcmeta"),
            (
                "minecraft-jar",
                "assets/example/textures/block/stone.png.mcmeta",
            ),
            (
                "mod-root",
                "assets/example/textures/block/stone.png.mcmeta.mcmeta",
            ),
        ] {
            assert!(!is_manifest_path_allowed(&allowed, source, path));
        }
        let model_paths = std::collections::HashSet::from([(
            "mod-root".to_owned(),
            "assets/example/models/block/stone.json".to_owned(),
        )]);
        assert!(!is_manifest_path_allowed(
            &model_paths,
            "mod-root",
            "assets/example/models/block/stone.json.mcmeta"
        ));
    }

    #[test]
    fn detects_version_jars_and_prioritizes_the_project_version() {
        let temporary = TemporaryDirectory::create();
        let versions = temporary.0.join(".minecraft").join("versions");
        let older = versions.join("1.20.1").join("1.20.1.jar");
        let active = versions.join("1.21.1").join("1.21.1.jar");
        write_jar(&older, true);
        write_jar(&active, true);

        let candidates = detect_minecraft_jars_in_versions_dir(&versions, Some("1.21"));

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].version, "1.21.1");
        assert_eq!(PathBuf::from(&candidates[0].path), active);
    }

    #[test]
    fn rejects_non_jar_and_archives_without_vanilla_models() {
        let temporary = TemporaryDirectory::create();
        let invalid = temporary.0.join("invalid.jar");
        fs::write(&invalid, b"not a zip").unwrap();
        let no_models = temporary.0.join("no-models.jar");
        write_jar(&no_models, false);
        let valid = temporary.0.join("minecraft.jar");
        write_jar(&valid, true);

        assert!(validate_minecraft_jar(&invalid).is_err());
        assert!(validate_minecraft_jar(&no_models).is_err());
        assert!(validate_minecraft_jar(&valid).is_ok());
    }

    #[test]
    fn deleting_open_project_closes_writer_lock_and_preview_archive_reader() {
        let temporary = TemporaryDirectory::create();
        let data_dir = temporary.0.join("app-data");
        let paths = AppPaths {
            data_dir: data_dir.clone(),
            settings_file: data_dir.join("settings.json"),
            recent_file: data_dir.join("recent.json"),
            logs_dir: data_dir.join("logs"),
            projects_dir: data_dir.join("projects"),
        };
        fs::create_dir_all(&paths.projects_dir).unwrap();
        let source_jar = temporary.0.join("source.jar");
        write_jar(&source_jar, false);
        let summary = yc_project::create_project(
            &paths.projects_dir,
            std::slice::from_ref(&source_jar),
            None,
        )
        .expect("Project should be created");
        let project_id = Uuid::parse_str(&summary.id).expect("Project id should be a UUID");
        let opened = yc_project::open_project(&paths.projects_dir, project_id)
            .expect("Project should stay open with its writer and lock");
        let project_directory = opened.directory().to_path_buf();
        let stored_source = opened.repository().mod_unit_source_path(1).unwrap();
        let context =
            build_preview_context(&summary.id, 1, &project_directory, &stored_source, None)
                .expect("preview context should retain the source JAR archive reader");
        assert!(context.readers.contains_key("mod-root"));

        let (_, log_guard) = tracing_appender::non_blocking(std::io::sink());
        let state = AppState::new(paths.clone(), log_guard);
        *state.active_project.lock().unwrap() = Some(opened);
        *state.preview_context.lock().unwrap() = Some(context);

        let _project_io = prepare_project_for_deletion(
            &state.project_io_lock,
            &state.active_project,
            &state.project_analysis_cache,
            &state.project_asset_index_cache,
            &state.preview_context,
            &summary.id,
        )
        .expect("delete preparation should close all project-owned handles");
        yc_project::delete_project(&paths.projects_dir, project_id)
            .expect("closed Project should be deleted");

        assert!(!project_directory.exists());
        assert!(state.active_project.lock().unwrap().is_none());
        assert!(state.preview_context.lock().unwrap().is_none());
    }
}
