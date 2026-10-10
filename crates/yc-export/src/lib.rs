//! Language File、Resource Pack、Modified JAR の書き出しを行う。

mod deployment;
mod modified_jar;
mod resource_pack;
mod validation;
mod version_detection;

use std::path::{Path, PathBuf};

use thiserror::Error;
use yc_language::{
    DiscoveredLanguageFile, LanguageFileFormat, TranslationEntry, WriterOptions,
    language_export_filename, write_language_file,
};

pub use deployment::{
    InPlaceReplaceReport, install_resource_pack, install_resource_pack_to,
    instance_dir_from_mods_dir, looks_like_instance_dir, replace_jar_in_place,
    replace_jar_in_place_with_backup, restore_jar_from_backup,
};
pub use modified_jar::{export_modified_jar, modified_jar_filename};
pub use resource_pack::{
    ManualPackFormat, MergedPackMod, MergedResourcePackConflict, MergedResourcePackOptions,
    MergedResourcePackReport, PackMetadataFormat, ResolvedPackFormat, ResourcePackMod,
    ResourcePackOptions, export_merged_resource_pack, export_resource_pack,
    merged_resource_pack_display_name, merged_resource_pack_filename, pack_format_for_version,
    resource_pack_filename,
};
pub use validation::{
    ExportKind, ExportValidationRequest, PlaceholderWarning, ValidationIssue, ValidationIssueCode,
    ValidationSeverity, validate_export,
};
pub use version_detection::{
    MinecraftVersionDetection, MinecraftVersionDetectionMethod, detect_minecraft_version,
    supported_minecraft_versions,
};

/// namespace単位で出力対象に含めるかを表す。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceExport {
    pub namespace: String,
    pub included: bool,
}

/// DBに依存しない、出力に必要な翻訳と解析情報。
#[derive(Clone, Debug)]
pub struct ExportInput<'a> {
    pub entries: &'a [TranslationEntry],
    pub namespaces: &'a [NamespaceExport],
    pub language_files: &'a [DiscoveredLanguageFile],
    /// Namespaces whose selected source `.lang` file uses Forge's `#PARSE_ESCAPES` mode.
    pub legacy_parse_escapes_namespaces: &'a [String],
    pub source_locale: &'a str,
    pub writer_options: WriterOptions,
    /// 解析側ですでに検出した不正ファイル。書き出し側で内容を推測しない。
    pub malformed_translation_files: &'a [String],
    /// Placeholder検証側ですでに検出した警告。
    pub placeholder_warnings: &'a [PlaceholderWarning],
}

/// 生成したLanguage Fileとそのnamespace。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageFileArtifact {
    pub namespace: String,
    pub file_name: String,
    pub bytes: Vec<u8>,
}

pub(crate) fn namespace_has_exportable_entries(input: &ExportInput<'_>, namespace: &str) -> bool {
    // Output membership follows the same empty-value policy as the writer, so empty namespaces
    // neither force source-file lookup nor produce language files with no translated entries.
    input.entries.iter().any(|entry| {
        entry.namespace == namespace
            && (input.writer_options.include_empty_translations || !entry.target_text.is_empty())
    })
}

pub(crate) fn writer_options_for_namespace(
    input: &ExportInput<'_>,
    namespace: &str,
) -> WriterOptions {
    WriterOptions {
        legacy_parse_escapes: input.writer_options.legacy_parse_escapes
            || input
                .legacy_parse_escapes_namespaces
                .iter()
                .any(|candidate| candidate == namespace),
        ..input.writer_options
    }
}

pub(crate) fn language_format_for_filename(
    namespace: &str,
    file_name: &str,
) -> Result<LanguageFileFormat, ExportError> {
    match file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("json") => Ok(LanguageFileFormat::Json),
        Some("lang") => Ok(LanguageFileFormat::LegacyLang),
        _ => Err(ExportError::UnsupportedLanguageFileFormat {
            namespace: namespace.to_owned(),
        }),
    }
}

/// Language Fileをnamespaceごとに生成する。
pub fn create_language_files(
    input: &ExportInput<'_>,
) -> Result<Vec<LanguageFileArtifact>, ExportError> {
    let validation = ExportValidationRequest {
        input,
        kind: ExportKind::LanguageFile,
        minecraft_version: None,
        manual_pack_format: None,
        original_jar_path: None,
    };
    if let Some(error) = validate_export(&validation)
        .into_iter()
        .find(|issue| issue.severity == ValidationSeverity::Error)
    {
        return Err(ExportError::ValidationFailed(error.message));
    }

    let mut namespaces: Vec<_> = input
        .namespaces
        .iter()
        .filter(|namespace| {
            namespace.included && namespace_has_exportable_entries(input, &namespace.namespace)
        })
        .collect();
    namespaces.sort_by(|left, right| left.namespace.cmp(&right.namespace));

    let mut files = Vec::with_capacity(namespaces.len());
    for namespace in namespaces {
        let file_name = language_export_filename(
            input.language_files,
            &namespace.namespace,
            input.source_locale,
        )
        .ok_or_else(|| ExportError::UnsupportedLanguageFileFormat {
            namespace: namespace.namespace.clone(),
        })?;
        if !language_filename_is_safe(&file_name) {
            return Err(ExportError::UnsafeArchivePath(file_name));
        }
        let format = language_format_for_filename(&namespace.namespace, &file_name)?;
        let bytes = write_language_file(
            &namespace.namespace,
            input.entries,
            format,
            writer_options_for_namespace(input, &namespace.namespace),
        )?;
        files.push(LanguageFileArtifact {
            namespace: namespace.namespace.clone(),
            file_name,
            bytes,
        });
    }
    Ok(files)
}

/// 生成済みLanguage Fileを個別にatomicに保存する。
pub fn write_language_files(
    output_directory: impl AsRef<Path>,
    files: &[LanguageFileArtifact],
    overwrite: bool,
) -> Result<Vec<PathBuf>, ExportError> {
    let output_directory = output_directory.as_ref();
    let mut written = Vec::with_capacity(files.len());
    for artifact in files {
        if !language_filename_is_safe(&artifact.file_name) {
            return Err(ExportError::UnsafeArchivePath(artifact.file_name.clone()));
        }
        if !namespace_component_is_safe(&artifact.namespace) {
            return Err(ExportError::UnsafeArchivePath(artifact.namespace.clone()));
        }
        // 同じ言語ファイル名でもnamespaceは別資産なので、出力先も分けて内容の上書きを防ぐ。
        let namespace_directory = output_directory.join(&artifact.namespace);
        std::fs::create_dir_all(&namespace_directory)?;
        let path = namespace_directory.join(&artifact.file_name);
        atomic_write_file(
            &path,
            overwrite,
            |file| {
                use std::io::Write;
                file.write_all(&artifact.bytes)?;
                Ok(())
            },
            |_| Ok(()),
        )?;
        written.push(path);
    }
    Ok(written)
}

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("language file format is unsupported for namespace {namespace}")]
    UnsupportedLanguageFileFormat { namespace: String },
    #[error("export validation failed: {0}")]
    ValidationFailed(String),
    #[error("output file already exists: {0}")]
    OutputExists(PathBuf),
    #[error("output path has no file name: {0}")]
    InvalidOutputPath(PathBuf),
    #[error("could not create temporary output next to {path}: {source}")]
    CreateTemporaryFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write output file {path}: {source}")]
    WriteOutput {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not finalize output file {path}: {source}")]
    FinalizeOutput {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not atomically move output to {path}: {source}")]
    RenameOutput {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not validate output archive {path}: {source}")]
    ValidateArchive {
        path: PathBuf,
        #[source]
        source: zip::result::ZipError,
    },
    #[error("output archive is missing generated entry {0}")]
    MissingGeneratedEntry(String),
    #[error("generated archive path is unsafe: {0}")]
    UnsafeArchivePath(String),
    #[error("duplicate target language entry in source archive: {0}")]
    DuplicateTargetEntry(String),
    #[error("mod {mod_name} could not be exported: {message}")]
    ModExportFailed { mod_name: String, message: String },
    #[error("namespace {namespace} uses both JSON and legacy language files")]
    MixedLanguageFileFormats { namespace: String },
    #[error("target JAR is locked and cannot be replaced: {0}")]
    TargetLocked(PathBuf),
    #[error("Minecraft version is required to choose the resource-pack language file name")]
    MissingMinecraftVersion,
    #[error("resource pack format is unknown for Minecraft {0}; provide a manual format")]
    UnknownPackFormat(String),
    #[error("signed JAR cannot be modified: {0}")]
    SignedJar(PathBuf),
    #[error("source JAR and output path refer to the same file: {0}")]
    SourceAndOutputSame(PathBuf),
    #[error("archive operation failed: {0}")]
    Archive(#[from] yc_archive::ArchiveError),
    #[error("language writer failed: {0}")]
    Language(#[from] yc_language::LanguageError),
    #[error("ZIP operation failed: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("JSON operation failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O operation failed: {0}")]
    Io(#[from] std::io::Error),
}

static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct TemporaryOutput {
    path: PathBuf,
    file: Option<std::fs::File>,
}

impl Drop for TemporaryOutput {
    fn drop(&mut self) {
        if self.file.is_some() {
            self.file.take();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn create_temporary_output(output: &Path) -> Result<TemporaryOutput, ExportError> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = output
        .file_name()
        .ok_or_else(|| ExportError::InvalidOutputPath(output.to_path_buf()))?;

    for _ in 0..100 {
        let counter = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut temp_name = std::ffi::OsString::from(".");
        temp_name.push(name);
        temp_name.push(format!(".yc-export-{}-{counter}.tmp", std::process::id()));
        let path = parent.join(temp_name);
        match std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                return Ok(TemporaryOutput {
                    path,
                    file: Some(file),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(ExportError::CreateTemporaryFile {
                    path: output.to_path_buf(),
                    source,
                });
            }
        }
    }
    Err(ExportError::CreateTemporaryFile {
        path: output.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "too many temporary-name collisions",
        ),
    })
}

fn atomic_write_file(
    output: &Path,
    overwrite: bool,
    write: impl FnOnce(&mut std::fs::File) -> Result<(), ExportError>,
    validate: impl FnOnce(&Path) -> Result<(), ExportError>,
) -> Result<(), ExportError> {
    if output.exists() && !overwrite {
        return Err(ExportError::OutputExists(output.to_path_buf()));
    }
    let mut temporary = create_temporary_output(output)?;
    let mut file = temporary
        .file
        .take()
        .expect("new temporary output always owns its file");
    write(&mut file).map_err(|error| match error {
        ExportError::Io(source) => ExportError::WriteOutput {
            path: output.to_path_buf(),
            source,
        },
        other => other,
    })?;
    use std::io::Write;
    file.flush().map_err(|source| ExportError::WriteOutput {
        path: output.to_path_buf(),
        source,
    })?;
    file.sync_all()
        .map_err(|source| ExportError::FinalizeOutput {
            path: output.to_path_buf(),
            source,
        })?;
    drop(file);
    validate(&temporary.path)?;

    if overwrite {
        std::fs::rename(&temporary.path, output).map_err(|source| ExportError::RenameOutput {
            path: output.to_path_buf(),
            source,
        })?;
    } else {
        publish_without_replacing(&temporary.path, output).map_err(|source| {
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                ExportError::OutputExists(output.to_path_buf())
            } else {
                ExportError::RenameOutput {
                    path: output.to_path_buf(),
                    source,
                }
            }
        })?;
    }
    temporary.file = None;
    Ok(())
}

fn publish_without_replacing(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // MoveFileWは宛先が存在すれば失敗するため、確認と移動の間に上書き競合が起きない。
        let moved = unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileW(
                source.as_ptr(),
                destination.as_ptr(),
            )
        };
        if moved == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(windows))]
    {
        // POSIXのlink作成は宛先を原子的に確保し、存在する宛先を置き換えない。
        std::fs::hard_link(source, destination)
    }
}

fn validate_zip(path: &Path, expected_entries: &[String]) -> Result<(), ExportError> {
    let file = std::fs::File::open(path).map_err(|source| ExportError::WriteOutput {
        path: path.to_path_buf(),
        source,
    })?;
    let archive = zip::ZipArchive::new(file).map_err(|source| ExportError::ValidateArchive {
        path: path.to_path_buf(),
        source,
    })?;
    let mut names = std::collections::HashSet::with_capacity(archive.len());
    for index in 0..archive.len() {
        if let Some(name) = archive.name_for_index(index) {
            names.insert(name.to_owned());
        }
    }
    for expected in expected_entries {
        if !names.contains(expected) {
            return Err(ExportError::MissingGeneratedEntry(expected.clone()));
        }
    }
    Ok(())
}

pub(crate) fn language_filename_is_safe(file_name: &str) -> bool {
    !file_name.is_empty()
        && !file_name
            .chars()
            .any(|character| matches!(character, '/' | '\\' | ':') || character.is_control())
        && file_name.rsplit_once('.').is_some_and(|(_, extension)| {
            extension.eq_ignore_ascii_case("json") || extension.eq_ignore_ascii_case("lang")
        })
}

fn namespace_component_is_safe(namespace: &str) -> bool {
    // Minecraft namespaceの文字に限定し、Windowsの末尾空白・ドット除去で親・現在ディレクトリ化する値を通さない。
    !namespace.is_empty()
        && !namespace.ends_with('.')
        && namespace.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        })
        && !matches!(namespace, "." | "..")
}

#[cfg(test)]
mod tests;
