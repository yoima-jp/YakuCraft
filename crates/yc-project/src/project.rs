use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use rusqlite::{Connection, params};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;
use yc_archive::{Archive, EntryKind};
use yc_language::{PatchouliArchiveFile, discover_books, extract_i18n_keys};

use crate::{
    ProjectError, ProjectLock, ProjectSummary, Repository, Result,
    database::{open_database, open_read_connection},
    repository,
    writer::ProjectWriter,
};

const DATABASE_FILE_NAME: &str = "project.sqlite";
const DELETE_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(50), Duration::from_millis(100)];

fn rename_project_directory(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

fn remove_project_directory(path: &Path) -> std::io::Result<()> {
    fs::remove_dir_all(path)
}

pub struct OpenedProject {
    directory: PathBuf,
    summary: ProjectSummary,
    writer: ProjectWriter,
    _lock: ProjectLock,
}

pub struct ModpackSource<'a> {
    pub game_directory: &'a Path,
    pub files: &'a BTreeMap<String, Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct ProjectSource {
    pub path: PathBuf,
    pub kind: crate::ModUnitSourceKind,
}

impl OpenedProject {
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn summary(&self) -> &ProjectSummary {
        &self.summary
    }

    pub fn refresh_summary(&mut self) -> Result<ProjectSummary> {
        self.summary = read_project_summary(&self.directory.join(DATABASE_FILE_NAME))?;
        Ok(self.summary.clone())
    }

    pub fn repository(&self) -> Repository {
        Repository::new(self.directory.join(DATABASE_FILE_NAME))
    }

    pub fn writer(&self) -> &ProjectWriter {
        &self.writer
    }

    pub fn rename(&mut self, display_name: &str) -> Result<ProjectSummary> {
        self.repository().rename_project(display_name)?;
        self.summary = read_project_summary(&self.directory.join(DATABASE_FILE_NAME))?;
        Ok(self.summary.clone())
    }

    pub fn remove_mod_unit(&mut self, mod_unit_id: i64) -> Result<ProjectSummary> {
        self.repository().remove_mod_unit(mod_unit_id)?;
        self.summary = read_project_summary(&self.directory.join(DATABASE_FILE_NAME))?;
        Ok(self.summary.clone())
    }

    pub fn add_mod_unit_source(
        &mut self,
        source: &ProjectSource,
    ) -> Result<Option<crate::ModUnitRecord>> {
        let repository = self.repository();
        let units = repository.list_mod_units()?;
        let filename = source_filename(&source.path)?;
        let original_path = fs::canonicalize(&source.path)?;
        let extension = source_extension(source.kind);
        let sources_directory = self.directory.join("sources");
        let pending_path =
            sources_directory.join(format!(".pending-{}.{}", Uuid::new_v4(), extension));
        copy_project_source(&source.path, source.kind, &pending_path)?;

        let result = (|| -> Result<Option<crate::ModUnitRecord>> {
            let source_sha256 = digest_to_hex(yc_archive::sha256_file(&pending_path)?);
            if repository.has_source_hash(&source_sha256)? {
                return Ok(None);
            }

            // 削除後にIDやsort_indexの欠番が残るため、件数ではなく最大値の次を使う。
            let id = units
                .iter()
                .map(|unit| unit.id)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| {
                    ProjectError::InvalidDatabaseValue("mod unit id overflow".to_owned())
                })?;
            let sort_index = match units.iter().map(|unit| unit.sort_index).max() {
                Some(maximum) => i64::try_from(maximum)
                    .ok()
                    .and_then(|value| value.checked_add(1))
                    .ok_or_else(|| {
                        ProjectError::InvalidDatabaseValue(
                            "mod unit sort index overflow".to_owned(),
                        )
                    })?,
                None => 0,
            };
            let stored_path = format!("sources/{id}.{extension}");
            let destination = self.directory.join(&stored_path);
            if destination.exists() {
                return Err(ProjectError::InvalidDatabaseValue(format!(
                    "stored source path already exists: {}",
                    destination.display()
                )));
            }
            fs::rename(&pending_path, &destination)?;
            let created_at = now_rfc3339()?;
            if let Err(error) = self
                .writer
                .add_mod_unit_source(repository::NewModUnitSource {
                    id,
                    source_kind: source.kind,
                    sort_index,
                    source_filename: filename,
                    source_sha256,
                    stored_path,
                    original_path: original_path.to_string_lossy().into_owned(),
                    created_at,
                })
            {
                let _ = fs::remove_file(&destination);
                return Err(error);
            }

            let unit = match repository
                .list_mod_units()?
                .into_iter()
                .find(|unit| unit.id == id)
            {
                Some(unit) => unit,
                None => {
                    let _ = self.writer.remove_mod_unit_preserving_project_name(id);
                    let _ = fs::remove_file(&destination);
                    return Err(ProjectError::InvalidDatabaseValue(format!(
                        "mod unit {id} was not registered"
                    )));
                }
            };
            if let Err(error) = self.refresh_summary() {
                let _ = self.writer.remove_mod_unit_preserving_project_name(id);
                let _ = fs::remove_file(&destination);
                return Err(error);
            }
            Ok(Some(unit))
        })();

        if pending_path.exists()
            && let Err(error) = fs::remove_file(&pending_path)
        {
            tracing::warn!(path = %pending_path.display(), %error, "pending source copy could not be removed");
        }
        result
    }
}

pub fn normalize_project_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ProjectError::InvalidProjectName(
            "プロジェクト名は1〜100文字で入力してください。".to_owned(),
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(ProjectError::InvalidProjectName(
            "プロジェクト名に制御文字は使えません。".to_owned(),
        ));
    }
    Ok(name.to_owned())
}

pub fn rename_project(
    projects_root: impl AsRef<Path>,
    project_id: Uuid,
    display_name: &str,
) -> Result<ProjectSummary> {
    let directory = projects_root.as_ref().join(project_id.to_string());
    if !directory.is_dir() {
        return Err(ProjectError::ProjectNotFound(directory));
    }
    let repository = crate::Repository::new(directory.join(DATABASE_FILE_NAME));
    repository.rename_project(display_name)?;
    read_project_summary(&directory.join(DATABASE_FILE_NAME))
}

pub fn create_project(
    projects_root: impl AsRef<Path>,
    sources: &[PathBuf],
    instance_path: Option<&Path>,
) -> Result<ProjectSummary> {
    create_project_with_modpack(projects_root, sources, instance_path, None)
}

pub fn create_project_with_modpack(
    projects_root: impl AsRef<Path>,
    sources: &[PathBuf],
    instance_path: Option<&Path>,
    modpack: Option<ModpackSource<'_>>,
) -> Result<ProjectSummary> {
    let sources: Vec<_> = sources
        .iter()
        .cloned()
        .map(|path| ProjectSource {
            path,
            kind: crate::ModUnitSourceKind::Jar,
        })
        .collect();
    create_project_with_sources(projects_root, &sources, instance_path, modpack)
}

pub fn create_project_with_sources(
    projects_root: impl AsRef<Path>,
    sources: &[ProjectSource],
    instance_path: Option<&Path>,
    modpack: Option<ModpackSource<'_>>,
) -> Result<ProjectSummary> {
    let projects_root = projects_root.as_ref();
    if sources.is_empty() && modpack.is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "at least one source JAR or modpack is required",
        )
        .into());
    }
    fs::create_dir_all(projects_root)?;

    let id = Uuid::new_v4();
    let project_directory = projects_root.join(id.to_string());
    fs::create_dir(&project_directory)?;
    let create_result = (|| -> Result<ProjectSummary> {
        fs::create_dir(project_directory.join("sources"))?;
        let database_path = project_directory.join(DATABASE_FILE_NAME);
        let mut connection = open_database(&database_path)?;
        let schema_version: i64 =
            connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let created_at = now_rfc3339()?;
        let transaction = connection.transaction()?;
        let initial_display_name = if modpack.is_none() && sources.len() == 1 {
            source_filename(&sources[0].path)?
        } else {
            instance_path
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| format!("{} 個の mod", sources.len()))
        };
        transaction.execute(
            "INSERT INTO project_meta (
                id, schema_version, created_at, updated_at, display_name, instance_path
            ) VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
            params![
                id.to_string(),
                schema_version,
                created_at,
                initial_display_name,
                instance_path.map(|path| path.to_string_lossy().into_owned())
            ],
        )?;

        for (index, source) in sources.iter().enumerate() {
            let unit_id = i64::try_from(index + 1)
                .map_err(|_| ProjectError::InvalidDatabaseValue("too many mod units".to_owned()))?;
            let filename = source_filename(&source.path)?;
            let original_path = fs::canonicalize(&source.path)?;
            let extension = source_extension(source.kind);
            let stored_path = format!("sources/{unit_id}.{extension}");
            let copied_source_path = project_directory.join(&stored_path);
            // 翻訳・再書き出しの基準をプロジェクト内に保持し、選択元ファイルは変更しない。
            copy_project_source(&source.path, source.kind, &copied_source_path)?;
            let source_sha256 = digest_to_hex(yc_archive::sha256_file(&copied_source_path)?);
            transaction.execute(
                "INSERT INTO mod_units (
                    id, source_kind, sort_index, source_filename, source_sha256, stored_path,
                    original_path, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    unit_id,
                    source.kind.as_db(),
                    i64::try_from(index).map_err(|_| {
                        ProjectError::InvalidDatabaseValue("too many mod units".to_owned())
                    })?,
                    filename,
                    source_sha256,
                    stored_path,
                    original_path.to_string_lossy(),
                    created_at,
                ],
            )?;
        }

        if let Some(modpack) = modpack {
            let unit_id = i64::try_from(sources.len() + 1)
                .map_err(|_| ProjectError::InvalidDatabaseValue("too many mod units".to_owned()))?;
            let sort_index = i64::try_from(sources.len())
                .map_err(|_| ProjectError::InvalidDatabaseValue("too many mod units".to_owned()))?;
            let stored_path = format!("sources/{unit_id}.zip");
            let archive_path = project_directory.join(&stored_path);
            let file = fs::File::create(&archive_path)?;
            let mut archive = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (path, bytes) in modpack.files {
                archive
                    .start_file(path, options)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                archive.write_all(bytes)?;
            }
            archive
                .finish()
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let source_sha256 = digest_to_hex(yc_archive::sha256_file(&archive_path)?);
            let original_path = fs::canonicalize(modpack.game_directory)?;
            transaction.execute(
                "INSERT INTO mod_units (
                    id, source_kind, sort_index, source_filename, source_sha256, stored_path,
                    original_path, created_at
                 ) VALUES (?1, 'modpack', ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    unit_id,
                    sort_index,
                    "Modpack 固有（クエスト・KubeJS）.zip",
                    source_sha256,
                    stored_path,
                    original_path.to_string_lossy(),
                    created_at,
                ],
            )?;
        }
        transaction.commit()?;
        read_project_summary(&database_path)
    })();

    if create_result.is_err()
        && let Err(cleanup_error) = fs::remove_dir_all(&project_directory)
    {
        tracing::warn!(path = %project_directory.display(), %cleanup_error, "failed to clean incomplete Project directory");
    }
    create_result
}

pub fn open_project(projects_root: impl AsRef<Path>, project_id: Uuid) -> Result<OpenedProject> {
    let directory = projects_root.as_ref().join(project_id.to_string());
    if !directory.is_dir() {
        return Err(ProjectError::ProjectNotFound(directory));
    }
    let lock = ProjectLock::acquire(&directory)?;
    let database_path = directory.join(DATABASE_FILE_NAME);
    let mut connection = open_database(&database_path)?;
    supplement_patchouli_text_flags(&mut connection, &directory)?;
    drop(connection);
    let summary = read_project_summary(&database_path)?;
    let writer = ProjectWriter::start(&database_path)?;
    Ok(OpenedProject {
        directory,
        summary,
        writer,
        _lock: lock,
    })
}

fn supplement_patchouli_text_flags(connection: &mut Connection, directory: &Path) -> Result<()> {
    let mod_units = {
        let mut statement = connection.prepare(
            "SELECT id, stored_path FROM mod_units WHERE source_kind = 'jar' ORDER BY sort_index, id",
        )?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };

    for (mod_unit_id, stored_path) in mod_units {
        let source_path = directory.join(stored_path);
        let mut archive = match Archive::open(&source_path) {
            Ok(archive) => archive,
            Err(error) => {
                tracing::warn!(path = %source_path.display(), %error, "Patchouli entry flags could not be supplemented");
                continue;
            }
        };
        let nested_paths = patchouli_nested_archive_paths(&mut archive);
        let mut keys = patchouli_i18n_keys(&mut archive);
        let mut nested_archives = match archive.discover_nested(&nested_paths) {
            Ok(nested_archives) => nested_archives,
            Err(error) => {
                tracing::warn!(path = %source_path.display(), %error, "nested Patchouli books could not be inspected");
                Vec::new()
            }
        };
        for nested in &mut nested_archives {
            let Some(archive) = nested.archive.as_mut() else {
                continue;
            };
            keys.extend(patchouli_i18n_keys(archive));
        }
        if !keys.is_empty() {
            let keys: Vec<_> = keys.into_iter().collect();
            crate::repository::add_patchouli_text_flags(connection, mod_unit_id, &keys)?;
        }
    }
    Ok(())
}

struct PatchouliMetadataLookup<'a, R> {
    archive: RefCell<&'a mut Archive<R>>,
    paths: Vec<String>,
}

impl<R: Read + Seek> yc_metadata::ArchiveLookup for PatchouliMetadataLookup<'_, R> {
    fn paths(&self) -> Vec<String> {
        self.paths.clone()
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.archive
            .borrow_mut()
            .read_entry(path, EntryKind::Text)
            .ok()
    }
}

fn patchouli_nested_archive_paths<R: Read + Seek>(archive: &mut Archive<R>) -> Vec<String> {
    let paths = archive
        .list_entries()
        .iter()
        .filter_map(|entry| entry.normalized_path.as_deref().map(str::to_owned))
        .collect();
    yc_metadata::analyze_metadata(&PatchouliMetadataLookup {
        archive: RefCell::new(archive),
        paths,
    })
    .nested_jars
}

fn patchouli_i18n_keys<R: Read + Seek>(archive: &mut Archive<R>) -> BTreeSet<(String, String)> {
    let paths: Vec<_> = archive
        .list_entries()
        .iter()
        .filter_map(|entry| entry.normalized_path.as_deref())
        .filter(|path| {
            path.ends_with(".json")
                && (path.starts_with("assets/") || path.starts_with("data/"))
                && path.contains("/patchouli_books/")
        })
        .map(str::to_owned)
        .collect();
    let mut owned_files = Vec::with_capacity(paths.len());
    for path in paths {
        match archive.read_entry(&path, EntryKind::Text) {
            Ok(bytes) => owned_files.push((path, bytes)),
            Err(error) => {
                tracing::warn!(%path, %error, "Patchouli file could not be read while supplementing entry flags");
            }
        }
    }
    let files: Vec<_> = owned_files
        .iter()
        .map(|(path, bytes)| PatchouliArchiveFile {
            path,
            bytes: bytes.as_slice(),
        })
        .collect();
    let mut keys = BTreeSet::new();
    for book in discover_books(&files).books {
        if book.i18n {
            let (book_keys, warnings) = extract_i18n_keys(&book);
            for warning in warnings {
                tracing::warn!(path = %warning.path, reason = %warning.reason, "Patchouli i18n key could not be extracted");
            }
            keys.extend(
                book_keys
                    .into_iter()
                    .map(|key| (book.namespace.clone(), key)),
            );
        }
    }
    keys
}

pub fn delete_project(projects_root: impl AsRef<Path>, project_id: Uuid) -> Result<()> {
    delete_project_with_operations(
        projects_root.as_ref(),
        project_id,
        rename_project_directory,
        remove_project_directory,
    )
}

/// 前回rename済みのProjectを起動時に回収する。一時ロックが残る場合は次回起動へ持ち越す。
pub fn cleanup_trash_directories(projects_root: impl AsRef<Path>) -> Result<()> {
    let projects_root = projects_root.as_ref();
    if !projects_root.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(projects_root)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(".trash-") || !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if let Err(error) = remove_directory_with_retry(&path, remove_project_directory) {
            tracing::warn!(path = %path.display(), %error, "deferred Project deletion cleanup failed");
        }
    }
    Ok(())
}

fn delete_project_with_operations(
    projects_root: &Path,
    project_id: Uuid,
    rename_directory: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
    remove_directory: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<()> {
    let directory = projects_root.join(project_id.to_string());
    if !directory.is_dir() {
        return Err(ProjectError::ProjectNotFound(directory));
    }
    let mut lock = ProjectLock::acquire(&directory)?;
    lock.stop_heartbeat();

    let trash_directory = projects_root.join(format!(".trash-{}", Uuid::new_v4()));
    // UUID名のProjectを先にrenameし、再帰削除に失敗しても不完全なProjectが一覧やopenに現れないようにする。
    rename_directory(&directory, &trash_directory).map_err(ProjectError::ProjectDeleteRename)?;
    lock.retarget_after_directory_move(&trash_directory);

    // Windowsでは開いたlock fileを含むdirectoryを削除できないため、lockの後始末を完了してから再帰削除する。
    drop(lock);
    // Windowsのウイルス対策ソフトなどが短時間だけファイルを開いている場合、rename後の掃除だけ再試行する。
    if let Err(error) = remove_directory_with_retry(&trash_directory, remove_directory) {
        tracing::warn!(path = %trash_directory.display(), %error, "Project moved to trash but cleanup was deferred");
    }
    Ok(())
}

fn remove_directory_with_retry(
    path: &Path,
    mut remove_directory: impl FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    for (attempt, delay) in DELETE_RETRY_DELAYS.iter().enumerate() {
        match remove_directory(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                tracing::debug!(path = %path.display(), attempt = attempt + 1, %error, "retrying deferred Project deletion cleanup");
                thread::sleep(*delay);
            }
        }
    }
    match remove_directory(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

pub fn list_projects(projects_root: impl AsRef<Path>) -> Result<Vec<ProjectSummary>> {
    let projects_root = projects_root.as_ref();
    if !projects_root.exists() {
        return Ok(Vec::new());
    }

    let mut projects = Vec::new();
    for entry in fs::read_dir(projects_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir()
            || Uuid::parse_str(&entry.file_name().to_string_lossy()).is_err()
        {
            continue;
        }
        let database_path = entry.path().join(DATABASE_FILE_NAME);
        match read_project_summary(&database_path) {
            Ok(summary) => projects.push(summary),
            Err(error) => {
                tracing::warn!(path = %database_path.display(), %error, "skipping unreadable Project during listing")
            }
        }
    }
    projects.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(projects)
}

fn read_project_summary(database_path: &Path) -> Result<ProjectSummary> {
    if !database_path.is_file() {
        return Err(ProjectError::ProjectNotFound(database_path.to_path_buf()));
    }
    let connection = open_read_connection(database_path)?;
    connection
        .query_row(
            "SELECT id, display_name, instance_path, created_at, updated_at,
                    (SELECT COUNT(*) FROM mod_units)
             FROM project_meta LIMIT 1",
            [],
            |row| {
                let id: String = row.get(0)?;
                let mod_count: i64 = row.get(5)?;
                Ok((
                    id,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    usize::try_from(mod_count)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, mod_count))?,
                ))
            },
        )
        .map(
            |(id, display_name, instance_path, created_at, updated_at, mod_count)| ProjectSummary {
                id,
                display_name,
                mod_count,
                instance_path,
                created_at,
                updated_at,
            },
        )
        .map_err(Into::into)
}

fn source_filename(source: &Path) -> Result<String> {
    source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "source JAR has no filename",
            )
            .into()
        })
}

fn source_extension(kind: crate::ModUnitSourceKind) -> &'static str {
    if kind == crate::ModUnitSourceKind::Shaderpack {
        "zip"
    } else {
        "jar"
    }
}

fn copy_project_source(
    source_path: &Path,
    kind: crate::ModUnitSourceKind,
    destination: &Path,
) -> Result<()> {
    if kind == crate::ModUnitSourceKind::Shaderpack && source_path.is_dir() {
        yc_shaderpack::pack_folder(source_path, destination)
            .map_err(|error| std::io::Error::other(error.to_string()).into())
    } else {
        fs::copy(source_path, destination)?;
        Ok(())
    }
}

fn digest_to_hex(digest: [u8; 32]) -> String {
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn now_rfc3339() -> Result<String> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, sync::mpsc, thread, time::Duration};

    use uuid::Uuid;

    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::{delete_project, delete_project_with_operations};
    use crate::{ProjectError, create_project, list_projects, open_project};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("yc-project-delete-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).expect("create temporary test directory");
            Self(path)
        }

        fn projects_root(&self) -> PathBuf {
            self.0.join("projects")
        }

        fn create_project(&self) -> (PathBuf, Uuid) {
            let source_jar = self.0.join("source.jar");
            fs::write(&source_jar, b"source jar").expect("write source fixture");
            let projects_root = self.projects_root();
            let project = create_project(&projects_root, std::slice::from_ref(&source_jar), None)
                .expect("create Project");
            (
                projects_root,
                Uuid::parse_str(&project.id).expect("parse Project UUID"),
            )
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn delete_project_stops_heartbeat_and_removes_directory() {
        let temporary = TestDirectory::new();
        let (projects_root, project_id) = temporary.create_project();
        let project_directory = projects_root.join(project_id.to_string());

        delete_project(&projects_root, project_id).expect("delete Project");

        assert!(!project_directory.exists());
        assert_eq!(fs::read_dir(&projects_root).unwrap().count(), 0);
    }

    #[test]
    fn open_is_rejected_after_project_is_renamed_during_deletion() {
        let temporary = TestDirectory::new();
        let (projects_root, project_id) = temporary.create_project();
        let projects_for_delete = projects_root.clone();
        let (renamed_sender, renamed_receiver) = mpsc::channel();
        let (continue_sender, continue_receiver) = mpsc::channel();

        let deletion = thread::spawn(move || {
            delete_project_with_operations(
                &projects_for_delete,
                project_id,
                super::rename_project_directory,
                |trash_directory| {
                    renamed_sender
                        .send(trash_directory.to_path_buf())
                        .expect("notify test after rename");
                    continue_receiver
                        .recv_timeout(Duration::from_secs(5))
                        .expect("continue deletion");
                    fs::remove_dir_all(trash_directory)
                },
            )
        });

        let trash_directory = renamed_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("wait for Project rename");
        assert!(trash_directory.is_dir());
        assert!(matches!(
            open_project(&projects_root, project_id),
            Err(ProjectError::ProjectNotFound(_))
        ));

        continue_sender.send(()).expect("resume deletion");
        deletion
            .join()
            .expect("join deletion thread")
            .expect("finish deletion");
        assert!(!trash_directory.exists());
    }

    #[test]
    fn failed_trash_removal_does_not_list_or_open_partial_project() {
        let temporary = TestDirectory::new();
        let (projects_root, project_id) = temporary.create_project();

        let result = delete_project_with_operations(
            &projects_root,
            project_id,
            super::rename_project_directory,
            |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "simulated removal failure",
                ))
            },
        );

        result.expect("rename commits the deletion even while cleanup is deferred");
        assert!(
            list_projects(&projects_root)
                .expect("list Projects after failed deletion")
                .is_empty()
        );
        assert!(matches!(
            open_project(&projects_root, project_id),
            Err(ProjectError::ProjectNotFound(_))
        ));
        let trash_directory = fs::read_dir(&projects_root)
            .expect("read projects directory")
            .map(|entry| entry.expect("read projects entry").path())
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".trash-"))
            })
            .expect("failed deletion should retain its trash directory");
        assert!(trash_directory.is_dir());
        super::cleanup_trash_directories(&projects_root).expect("sweep deferred trash");
        assert!(!trash_directory.exists());
    }

    #[test]
    fn transient_cleanup_failure_is_retried() {
        let temporary = TestDirectory::new();
        let (projects_root, project_id) = temporary.create_project();
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_cleanup = Arc::clone(&attempts);

        delete_project_with_operations(
            &projects_root,
            project_id,
            super::rename_project_directory,
            |path| {
                if attempts_for_cleanup.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "simulated transient lock",
                    ))
                } else {
                    super::remove_project_directory(path)
                }
            },
        )
        .expect("transient cleanup failure is retried");

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(fs::read_dir(&projects_root).unwrap().count(), 0);
    }

    #[test]
    fn rename_failure_preserves_the_project_and_reports_its_phase() {
        let temporary = TestDirectory::new();
        let (projects_root, project_id) = temporary.create_project();
        let project_directory = projects_root.join(project_id.to_string());

        let result = delete_project_with_operations(
            &projects_root,
            project_id,
            |_, _| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "locked",
                ))
            },
            super::remove_project_directory,
        );

        assert!(matches!(result, Err(ProjectError::ProjectDeleteRename(_))));
        assert!(project_directory.is_dir());
    }
}
