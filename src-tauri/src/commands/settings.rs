use std::{collections::HashMap, fs, path::Path, sync::Mutex};

use serde_json::{Value, json};
use tauri::State;
use uuid::Uuid;
use yc_project::{AppSettings, ProjectError, load_settings, save_settings as save_app_settings};

use crate::{error::AppError, paths::AppPaths, state::AppState};

static SETTINGS_FILE_LOCK: Mutex<()> = Mutex::new(());

/// 設定の部分更新を一つのロック内で完了し、別の設定を同時保存しても消さない。
/// `yc_project` の保存処理は一時ファイルを書いてから置き換えるため、JSONも途中状態にならない。
pub(crate) fn update_settings_file<E>(
    path: impl AsRef<Path>,
    update: impl FnOnce(&mut AppSettings) -> Result<(), E>,
    map_load_error: impl Fn(ProjectError) -> E,
    map_save_error: impl Fn(ProjectError) -> E,
) -> Result<AppSettings, E> {
    update_settings_file_if(
        path,
        |settings| update(settings).map(|()| true),
        map_load_error,
        map_save_error,
    )
}

fn update_settings_file_if<E>(
    path: impl AsRef<Path>,
    update: impl FnOnce(&mut AppSettings) -> Result<bool, E>,
    map_load_error: impl Fn(ProjectError) -> E,
    map_save_error: impl Fn(ProjectError) -> E,
) -> Result<AppSettings, E> {
    let _guard = SETTINGS_FILE_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let path = path.as_ref();
    let mut current = load_settings(path).map_err(map_load_error)?;
    if update(&mut current)? {
        save_app_settings(path, &current).map_err(map_save_error)?;
    }
    Ok(current)
}

/// 一度だけ既存環境を判定し、設定をロック内で移行する。
pub(crate) fn migrate_settings(paths: &AppPaths) {
    if let Err(error) = migrate_settings_files(&paths.settings_file, &paths.projects_dir) {
        tracing::warn!(%error, "settings migration could not be applied");
    }
}

fn migrate_settings_files(settings_file: &Path, projects_dir: &Path) -> Result<(), ProjectError> {
    let settings_file_existed = settings_file.exists();
    update_settings_file_if(
        settings_file,
        |settings| {
            if settings.values.contains_key("onboarding.completed") {
                return Ok(false);
            }

            let existing_environment =
                settings_file_existed || contains_project_database(projects_dir)?;
            if existing_environment {
                settings
                    .values
                    .entry("language.uiLanguage".to_owned())
                    .or_insert_with(|| json!("ja"));
                settings
                    .values
                    .entry("language.targetLocale".to_owned())
                    .or_insert_with(|| json!("ja_jp"));
                settings
                    .values
                    .insert("onboarding.completed".to_owned(), json!(true));
            } else {
                // falseを保存して判定結果を固定し、オンボーディング中の別設定保存で
                // 次回起動時に既存環境として誤判定されないようにする。
                settings
                    .values
                    .insert("onboarding.completed".to_owned(), json!(false));
            }
            Ok(true)
        },
        |error| error,
        |error| error,
    )?;
    Ok(())
}

fn contains_project_database(projects_dir: &Path) -> Result<bool, ProjectError> {
    // yc-projectのproject.rsで使うデータベース名。
    const DATABASE_FILE_NAME: &str = "project.sqlite";

    for entry in fs::read_dir(projects_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Ok(project_id) = Uuid::parse_str(file_name) else {
            continue;
        };
        if !project_id.to_string().eq_ignore_ascii_case(file_name) {
            continue;
        }
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let database = entry.path().join(DATABASE_FILE_NAME);
        match fs::symlink_metadata(database) {
            Ok(metadata) if metadata.is_file() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, AppError> {
    load_settings(&state.paths.settings_file).map_err(settings_error)
}

#[tauri::command]
pub fn save_settings(
    settings: HashMap<String, Value>,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let recent_projects_count_changed = settings.contains_key("general.recentProjectsCount");
    let mut ui_language_changed = false;
    let updated = update_settings_file(
        &state.paths.settings_file,
        |current| {
            for (key, value) in settings {
                if key != "schemaVersion" {
                    if key == "language.uiLanguage" {
                        ui_language_changed = current.values.get(&key) != Some(&value);
                    }
                    current.values.insert(key, value);
                }
            }
            Ok(())
        },
        settings_error,
        settings_error,
    )?;
    if ui_language_changed {
        let language = updated
            .values
            .get("language.uiLanguage")
            .and_then(Value::as_str)
            .and_then(yc_locale::UiLanguage::parse)
            .unwrap_or(yc_locale::UiLanguage::En);
        yc_locale::set_ui_language(language);
    }

    if recent_projects_count_changed {
        // 件数を減らした場合も最近の一覧とJSONの保持数を同じ操作でそろえる。
        match state.recent_lock.lock() {
            Ok(_recent_guard) => {
                if let Err(error) = super::project::trim_recent_projects_to_setting(&state.paths) {
                    tracing::warn!(%error.user_message, "recent projects retention could not be updated");
                }
            }
            Err(_) => tracing::warn!("recent projects lock was poisoned while applying retention"),
        }
    }
    Ok(())
}

#[tauri::command]
pub fn clear_cache(state: State<'_, AppState>) -> Result<(), AppError> {
    clear_cache_contents(&state)
}

fn clear_cache_contents(state: &AppState) -> Result<(), AppError> {
    // サムネイルの読み書きやプロジェクト削除と直列化し、開いたプロジェクトの書き込み競合を防ぐ。
    let _project_io = state
        .project_io_lock
        .lock()
        .map_err(|error| cache_error(error.to_string()))?;

    remove_cache_directory(&state.paths.data_dir.join("cache"))?;

    let project_entries = match fs::read_dir(&state.paths.projects_dir) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(cache_error(error.to_string())),
    };
    if let Some(project_entries) = project_entries {
        for entry in project_entries {
            let entry = entry.map_err(|error| cache_error(error.to_string()))?;
            let file_type = entry
                .file_type()
                .map_err(|error| cache_error(error.to_string()))?;
            if !file_type.is_dir() || Uuid::parse_str(&entry.file_name().to_string_lossy()).is_err()
            {
                continue;
            }
            remove_cache_directory(&super::preview::project_cache_directory(&entry.path()))?;
        }
    }

    *state
        .project_analysis_cache
        .lock()
        .map_err(|error| cache_error(error.to_string()))? = None;
    *state
        .project_asset_index_cache
        .lock()
        .map_err(|error| cache_error(error.to_string()))? = None;
    super::preview::clear_mod_icon_cache().map_err(|error| cache_error(error.user_message))?;

    // preview_context は表示中のアセット読込に使うため保持する。削除対象は再生成できるファイルと解析キャッシュ。
    Ok(())
}

fn remove_cache_directory(path: &Path) -> Result<(), AppError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(cache_error(error.to_string())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppError::simple(
            "cache_unavailable",
            "キャッシュを削除できませんでした。",
        ));
    }
    fs::remove_dir_all(path).map_err(|error| cache_error(error.to_string()))
}

fn cache_error(detail: impl std::fmt::Display) -> AppError {
    AppError::new(
        "cache_unavailable",
        "キャッシュを削除できませんでした。",
        Some(detail.to_string()),
    )
}

fn settings_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        "settings_unavailable",
        "設定を読み書きできませんでした。",
        Some(error.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, sync::mpsc, thread, time::Duration};

    use serde_json::json;
    use uuid::Uuid;
    use yc_project::{load_settings, open_project};

    use super::{clear_cache_contents, migrate_settings_files, update_settings_file};
    use crate::{paths::AppPaths, state::AppState};

    #[test]
    fn concurrent_general_and_minecraft_jar_updates_preserve_both_values() {
        let temporary = TemporaryDirectory::create();
        let settings_file = temporary.0.join("settings.json");
        let (first_loaded_tx, first_loaded_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let first_settings_file = settings_file.clone();
        let first = thread::spawn(move || {
            update_settings_file(
                first_settings_file,
                |settings| {
                    settings
                        .values
                        .insert("general.autoOpenOutputFolder".to_owned(), json!(true));
                    first_loaded_tx
                        .send(())
                        .expect("test should be waiting for the first update");
                    release_first_rx
                        .recv()
                        .expect("test should release the first update");
                    Ok(())
                },
                |error| error,
                |error| error,
            )
            .expect("general setting update should save");
        });
        first_loaded_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first update should reach its mutation while holding the settings lock");

        let (second_mutated_tx, second_mutated_rx) = mpsc::channel();
        let (second_done_tx, second_done_rx) = mpsc::channel();
        let second_settings_file = settings_file.clone();
        let second = thread::spawn(move || {
            update_settings_file(
                second_settings_file,
                |settings| {
                    settings.values.insert(
                        "preview.minecraftJarPath".to_owned(),
                        json!("/minecraft/selected.jar"),
                    );
                    second_mutated_tx
                        .send(())
                        .expect("test should observe the second update");
                    Ok(())
                },
                |error| error,
                |error| error,
            )
            .expect("Minecraft JAR setting update should save");
            second_done_tx
                .send(())
                .expect("test should observe the completed second update");
        });

        // If the shared lock is removed, the second update reaches its mutation while the
        // first still holds a stale snapshot, recreating the lost-update window deterministically.
        let second_mutated_before_first_commit = second_mutated_rx
            .recv_timeout(Duration::from_millis(500))
            .is_ok();
        if second_mutated_before_first_commit {
            second_done_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("unlocked second update should finish before releasing the first");
        }
        release_first_tx
            .send(())
            .expect("first update should be released");
        if !second_mutated_before_first_commit {
            second_done_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("second update should finish after the first commit");
        }
        first.join().expect("first setting worker should finish");
        second.join().expect("second setting worker should finish");
        assert!(
            !second_mutated_before_first_commit,
            "setting updates must not enter the mutation phase concurrently"
        );

        let saved = load_settings(&settings_file).expect("settings should remain valid");
        assert_eq!(saved.values["general.autoOpenOutputFolder"], json!(true));
        assert_eq!(
            saved.values["preview.minecraftJarPath"],
            json!("/minecraft/selected.jar")
        );
    }

    #[test]
    fn clearing_cache_removes_all_project_thumbnails_and_keeps_open_project_data() {
        let temporary = TemporaryDirectory::create();
        let data_dir = temporary.0.join("app-data");
        let projects_dir = data_dir.join("projects");
        fs::create_dir_all(&projects_dir).expect("projects directory should exist");

        let source_jar = temporary.0.join("source.jar");
        fs::write(&source_jar, b"source jar fixture").expect("source JAR should be created");
        let summary = yc_project::create_project(&projects_dir, &[source_jar], None)
            .expect("project should be created");
        let project_id = Uuid::parse_str(&summary.id).expect("project id should be a UUID");
        let opened = open_project(&projects_dir, project_id).expect("project should open");
        let project_directory = opened.directory().to_path_buf();
        let project_cache = super::super::preview::project_cache_directory(&project_directory);
        let thumbnails = project_cache.join("thumbnails");
        fs::create_dir_all(&thumbnails).expect("thumbnail cache should be created");
        fs::write(thumbnails.join("entry.png"), b"thumbnail")
            .expect("thumbnail fixture should be created");

        // DBを一覧できない状態でも、UUIDのプロジェクトフォルダーに残ったキャッシュは削除する。
        let incomplete_project = projects_dir.join(Uuid::new_v4().to_string());
        let incomplete_cache = super::super::preview::project_cache_directory(&incomplete_project);
        fs::create_dir_all(incomplete_cache.join("thumbnails"))
            .expect("incomplete project's cache should be created");
        fs::write(incomplete_cache.join("thumbnails/entry.png"), b"thumbnail")
            .expect("incomplete thumbnail fixture should be created");

        let paths = AppPaths {
            data_dir: data_dir.clone(),
            settings_file: data_dir.join("settings.json"),
            recent_file: data_dir.join("recent.json"),
            logs_dir: data_dir.join("logs"),
            projects_dir: projects_dir.clone(),
        };
        fs::create_dir_all(data_dir.join("cache")).expect("application cache should be created");
        fs::write(data_dir.join("cache/generated"), b"generated")
            .expect("application cache fixture should be created");

        let (_, log_guard) = tracing_appender::non_blocking(std::io::sink());
        let state = AppState::new(paths, log_guard);
        *state.active_project.lock().expect("active project lock") = Some(opened);
        clear_cache_contents(&state).expect("cache clear should succeed");

        assert!(!data_dir.join("cache").exists());
        assert!(!project_cache.exists());
        assert!(!incomplete_cache.exists());
        assert!(project_directory.join("project.sqlite").is_file());
        assert!(project_directory.join("sources/1.jar").is_file());
        {
            let active = state.active_project.lock().expect("active project lock");
            let active = active.as_ref().expect("project should remain open");
            assert_eq!(active.directory(), project_directory);
            active
                .repository()
                .list_entries(None)
                .expect("open project should remain usable");
        }

        // キャッシュを消した後も、通常の描画処理が同じ場所へサムネイルを再生成できる。
        let regenerated_thumbnail =
            super::super::preview::project_cache_directory(&project_directory)
                .join("thumbnails")
                .join("entry.png");
        fs::create_dir_all(regenerated_thumbnail.parent().expect("thumbnail parent"))
            .expect("thumbnail cache should be recreated on demand");
        fs::write(regenerated_thumbnail, b"regenerated")
            .expect("thumbnail can be regenerated for the open project");
    }

    #[test]
    fn new_install_with_empty_projects_and_recent_file_stays_in_onboarding() {
        let temporary = TemporaryDirectory::create();
        let projects_dir = temporary.0.join("projects");
        fs::create_dir_all(&projects_dir).expect("projects directory should exist");
        let settings_file = temporary.0.join("settings.json");
        fs::write(temporary.0.join("recent.json"), b"{}").expect("recent file should exist");

        migrate_settings_files(&settings_file, &projects_dir)
            .expect("new installation migration should succeed");

        let settings = load_settings(settings_file).expect("settings should be valid");
        assert_eq!(settings.values["onboarding.completed"], json!(false));
        assert!(!settings.values.contains_key("language.uiLanguage"));
        assert!(!settings.values.contains_key("language.targetLocale"));
    }

    #[test]
    fn existing_settings_file_completes_onboarding_and_uses_japanese_defaults() {
        let temporary = TemporaryDirectory::create();
        let projects_dir = temporary.0.join("projects");
        fs::create_dir_all(&projects_dir).expect("projects directory should exist");
        let settings_file = temporary.0.join("settings.json");
        fs::write(&settings_file, b"{\"advanced.debugLogs\":true}")
            .expect("existing settings should be written");

        migrate_settings_files(&settings_file, &projects_dir)
            .expect("existing installation migration should succeed");

        let settings = load_settings(settings_file).expect("settings should be valid");
        assert_eq!(settings.values["onboarding.completed"], json!(true));
        assert_eq!(settings.values["language.uiLanguage"], json!("ja"));
        assert_eq!(settings.values["language.targetLocale"], json!("ja_jp"));
        assert_eq!(settings.values["advanced.debugLogs"], json!(true));
    }

    #[test]
    fn project_database_without_settings_identifies_an_existing_installation() {
        let temporary = TemporaryDirectory::create();
        let projects_dir = temporary.0.join("projects");
        let project_dir = projects_dir.join(Uuid::new_v4().to_string());
        fs::create_dir_all(&project_dir).expect("project directory should exist");
        fs::write(project_dir.join("project.sqlite"), b"project database")
            .expect("database file should exist");
        let settings_file = temporary.0.join("settings.json");

        migrate_settings_files(&settings_file, &projects_dir)
            .expect("existing project migration should succeed");

        let settings = load_settings(settings_file).expect("settings should be valid");
        assert_eq!(settings.values["onboarding.completed"], json!(true));
        assert_eq!(settings.values["language.uiLanguage"], json!("ja"));
        assert_eq!(settings.values["language.targetLocale"], json!("ja_jp"));
    }

    #[test]
    fn corrupt_settings_are_left_unchanged() {
        let temporary = TemporaryDirectory::create();
        let projects_dir = temporary.0.join("projects");
        fs::create_dir_all(&projects_dir).expect("projects directory should exist");
        let settings_file = temporary.0.join("settings.json");
        let contents = b"{broken json";
        fs::write(&settings_file, contents).expect("corrupt settings should be written");

        assert!(migrate_settings_files(&settings_file, &projects_dir).is_err());
        assert_eq!(
            fs::read(settings_file).expect("settings should remain present"),
            contents
        );
    }

    #[test]
    fn migration_is_a_noop_after_completion_and_preserves_existing_values() {
        let temporary = TemporaryDirectory::create();
        let projects_dir = temporary.0.join("projects");
        fs::create_dir_all(&projects_dir).expect("projects directory should exist");
        let settings_file = temporary.0.join("settings.json");
        fs::write(
            &settings_file,
            br#"{"language.uiLanguage":"ko","language.targetLocale":"ko_kr","other.setting":7}"#,
        )
        .expect("existing settings should be written");

        migrate_settings_files(&settings_file, &projects_dir)
            .expect("first migration should succeed");
        let migrated = fs::read(&settings_file).expect("migrated settings should exist");
        migrate_settings_files(&settings_file, &projects_dir)
            .expect("second migration should succeed");

        assert_eq!(
            fs::read(&settings_file).expect("settings should exist"),
            migrated
        );
        let settings = load_settings(settings_file).expect("settings should be valid");
        assert_eq!(settings.values["onboarding.completed"], json!(true));
        assert_eq!(settings.values["language.uiLanguage"], json!("ko"));
        assert_eq!(settings.values["language.targetLocale"], json!("ko_kr"));
        assert_eq!(settings.values["other.setting"], json!(7));
    }

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn create() -> Self {
            let path = std::env::temp_dir().join(format!("yakucraft-settings-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).expect("temporary directory should be created");
            Self(path)
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
