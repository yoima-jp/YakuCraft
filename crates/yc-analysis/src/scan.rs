//! Modpack内JARの日本語翻訳状況を軽量に調べる。

use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use yc_archive::{Archive, ArchiveLimits, EntryKind, NestedDiscoveryError};
use yc_language::{
    LanguageArchivePath, PatchouliArchiveFile, discover_books, discover_language_files,
    extract_entries, normalize_locale, parse_language_file,
};
use yc_metadata::{ModInfo, analyze_metadata};

use crate::{AnalysisError, CancellationToken};

const HASH_BUFFER_SIZE: usize = 64 * 1024;
const MAX_SCAN_WORKERS: usize = 4;
const MAX_SCAN_ENTRIES: u64 = 50_000;
const MAX_SCAN_PATH_METADATA_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SCAN_TEXT_SIZE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SCAN_CUMULATIVE_READ_BYTES: u64 = 16 * 1024 * 1024;

/// 軽量スキャンの設定。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanOptions {
    pub target_locale: String,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            target_locale: "ja_jp".to_owned(),
        }
    }
}

/// JAR内で見つかった言語ファイルの翻訳状況。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModLanguageStatus {
    Untranslated,
    Partial { coverage: f64 },
    Complete,
    NoLanguageFiles,
    Failed { reason: String },
}

/// modsフォルダ内の1つのJARのスキャン結果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModScanResult {
    pub path: PathBuf,
    pub file_name: String,
    pub file_size_bytes: Option<u64>,
    pub sha256: Option<String>,
    pub mods: Vec<ModInfo>,
    pub source_key_count: usize,
    pub translated_key_count: usize,
    pub status: ModLanguageStatus,
}

/// 1つのJARが完了するたびに通知する進捗。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    pub completed: usize,
    pub total: usize,
    pub current_file: String,
}

/// 指定フォルダ直下のJARを並列スキャンし、ファイル名順に返す。
pub fn scan_mods_folder(
    dir: &Path,
    options: &ScanOptions,
    cancellation: &CancellationToken,
    on_progress: impl Fn(ScanProgress) + Sync,
) -> Result<Vec<ModScanResult>, AnalysisError> {
    ensure_not_cancelled(cancellation)?;

    let mut paths = Vec::new();
    let entries = fs::read_dir(dir).map_err(|source| AnalysisError::ScanFolderIo {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        ensure_not_cancelled(cancellation)?;
        let entry = entry.map_err(|source| AnalysisError::ScanFolderIo {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        // `foo.jar` という名前のディレクトリも拡張子だけでは候補に入り、Failed として表示されてしまう。
        // シンボリックリンク先のjarは対象に含めたいので、file_typeではなくmetadata（リンク解決後）で判定する。
        let is_file = fs::metadata(&path).is_ok_and(|metadata| metadata.is_file());
        if is_file
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
        {
            paths.push(path);
        }
    }

    paths.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    ensure_not_cancelled(cancellation)?;

    let total = paths.len();
    if total == 0 {
        return Ok(Vec::new());
    }

    let worker_count = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(MAX_SCAN_WORKERS)
        .min(total);
    let next_index = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let results = Mutex::new(
        std::iter::repeat_with(|| None)
            .take(total)
            .collect::<Vec<_>>(),
    );
    let progress_lock = Mutex::new(());

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let paths = &paths;
            let results = &results;
            let progress_lock = &progress_lock;
            let next_index = &next_index;
            let completed = &completed;
            let on_progress = &on_progress;
            scope.spawn(move || {
                loop {
                    if cancellation.is_cancelled() {
                        break;
                    }
                    let index = next_index.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = paths.get(index) else {
                        break;
                    };
                    let result = match scan_mod_jar_inner(path, options, Some(cancellation)) {
                        Ok(result) => result,
                        Err(AnalysisError::Cancelled) => break,
                        Err(error) => failed_result(path, error.to_string()),
                    };

                    let mut results = results
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    results[index] = Some(result);
                    drop(results);

                    let _progress_guard = progress_lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    on_progress(ScanProgress {
                        completed: done,
                        total,
                        current_file: file_name(path),
                    });
                }
            });
        }
    });

    ensure_not_cancelled(cancellation)?;
    let results = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(results
        .into_iter()
        .flatten()
        .collect::<Vec<ModScanResult>>())
}

/// 1つのJARをスキャンする。modsフォルダ全体の並列処理ではこの関数を単位に使う。
pub fn scan_mod_jar(path: &Path, options: &ScanOptions) -> ModScanResult {
    scan_mod_jar_inner(path, options, None)
        .unwrap_or_else(|error| failed_result(path, error.to_string()))
}

fn scan_mod_jar_inner(
    path: &Path,
    options: &ScanOptions,
    cancellation: Option<&CancellationToken>,
) -> Result<ModScanResult, AnalysisError> {
    ensure_optional_cancellation(cancellation)?;
    let file_size_bytes = match fs::metadata(path) {
        Ok(metadata) => Some(metadata.len()),
        Err(error) => return Ok(failed_result(path, error.to_string())),
    };

    let sha256 = match hash_file(path, cancellation) {
        Ok(hash) => Some(hash),
        Err(AnalysisError::Cancelled) => return Err(AnalysisError::Cancelled),
        Err(error) => {
            return Ok(failed_result_with_size(
                path,
                file_size_bytes,
                error.to_string(),
            ));
        }
    };

    let mut archive = match Archive::open_with_limits(path, scan_archive_limits()) {
        Ok(archive) => archive,
        Err(error) => {
            let mut result = failed_result(path, error.to_string());
            result.file_size_bytes = file_size_bytes;
            result.sha256 = sha256;
            return Ok(result);
        }
    };
    ensure_optional_cancellation(cancellation)?;

    // yc-metadata only needs actual metadata files and one root asset path per namespace. Keeping
    // this lookup small avoids cloning every central-directory path alongside Archive's index.
    let mut metadata_paths = Vec::new();
    let mut seen_namespaces = HashSet::new();
    let mut language_paths = Vec::new();
    for entry in archive.list_entries() {
        ensure_optional_cancellation(cancellation)?;
        let Some(path) = entry.normalized_path.as_deref() else {
            continue;
        };
        if is_metadata_path(path) {
            metadata_paths.push(path.to_owned());
        }
        if let Some(namespace) = root_asset_namespace(path)
            && seen_namespaces.insert(namespace.to_owned())
        {
            metadata_paths.push(path.to_owned());
        }
        if is_language_candidate(path) {
            language_paths.push(LanguageArchivePath::root(path));
        }
    }

    let metadata = {
        let lookup = super::ArchiveLookupAdapter::new(&mut archive, metadata_paths);
        analyze_metadata(&lookup)
    };

    let mut result = ModScanResult {
        path: path.to_path_buf(),
        file_name: file_name(path),
        file_size_bytes,
        sha256,
        mods: metadata.mods,
        source_key_count: 0,
        translated_key_count: 0,
        status: ModLanguageStatus::NoLanguageFiles,
    };

    let mut keys_by_namespace = BTreeMap::<String, BTreeMap<String, HashSet<String>>>::new();
    let mut target_keys_by_namespace = BTreeMap::<String, HashSet<String>>::new();
    let mut seen_target_keys_by_namespace = BTreeMap::<String, HashSet<String>>::new();
    let target_locale = normalize_locale(options.target_locale.trim());
    let mut first_language_error = None;
    let mut nested = Vec::<yc_archive::NestedArchive>::new();
    // Read root locales first: a large bundled dependency may exhaust the shared budget,
    // but must not hide usable root translations. Nested reads retain the same size limits.
    for include_nested in [false, true] {
        if include_nested {
            language_paths.clear();
            // Full analysis also discovers metadata-declared JARs outside the conventional directories.
            // The archive API shares the root read budget and checks cancellation during nested expansion.
            nested = archive
                .discover_nested_with_cancellation(&metadata.nested_jars, || {
                    cancellation.is_some_and(CancellationToken::is_cancelled)
                })
                .map_err(|error| match error {
                    NestedDiscoveryError::Cancelled => AnalysisError::Cancelled,
                    NestedDiscoveryError::Archive(error) => AnalysisError::Archive(error),
                })?;
            nested.sort_by(|left, right| left.path.cmp(&right.path));
            for candidate in &nested {
                ensure_optional_cancellation(cancellation)?;
                let Some(nested_archive) = &candidate.archive else {
                    continue;
                };
                for entry in nested_archive.list_entries() {
                    ensure_optional_cancellation(cancellation)?;
                    if let Some(path) = entry.normalized_path.as_deref()
                        && is_language_candidate(path)
                    {
                        language_paths.push(LanguageArchivePath::nested(&candidate.path, path));
                    }
                }
            }
        }
        for file in discover_language_files(&language_paths) {
            ensure_optional_cancellation(cancellation)?;
            let archive_path = file.archive_path.clone();
            let read = if file.container_path.is_empty() {
                archive.read_entry(&archive_path, EntryKind::Text)
            } else {
                nested
                    .iter_mut()
                    .find(|candidate| candidate.path == file.container_path)
                    .and_then(|candidate| candidate.archive.as_mut())
                    .expect("discovered language path belongs to an opened nested archive")
                    .read_entry(&archive_path, EntryKind::Text)
            };
            ensure_optional_cancellation(cancellation)?;
            let bytes = match read {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(path = %path.display(), archive_path, error = %error, "軽量JARスキャンで言語ファイルを読み取れません");
                    first_language_error.get_or_insert_with(|| format!("{archive_path}: {error}"));
                    continue;
                }
            };
            let parsed = match parse_language_file(file, &bytes) {
                Ok(parsed) => parsed,
                Err(error) => {
                    tracing::warn!(path = %path.display(), archive_path, error = %error, "軽量JARスキャンで言語ファイルを解析できません");
                    // Full analysis reports malformed locales as warnings and keeps usable locales;
                    // mirror that behavior so one broken translation file cannot hide an otherwise readable mod.
                    first_language_error.get_or_insert_with(|| format!("{archive_path}: {error}"));
                    continue;
                }
            };
            ensure_optional_cancellation(cancellation)?;

            let namespace = parsed.file.namespace;
            let locale = parsed.file.normalized_locale;
            let locale_keys = keys_by_namespace
                .entry(namespace.clone())
                .or_default()
                .entry(locale.clone())
                .or_default();
            for (key, value) in parsed.entries {
                locale_keys.insert(key.clone());
                if locale == target_locale
                    && seen_target_keys_by_namespace
                        .entry(namespace.clone())
                        .or_default()
                        .insert(key.clone())
                    && value
                        .text
                        .as_deref()
                        .is_some_and(|translation| !translation.trim().is_empty())
                {
                    target_keys_by_namespace
                        .entry(namespace.clone())
                        .or_default()
                        .insert(key);
                }
            }
        }
    }

    let (patchouli_source_count, patchouli_translated_count) = scan_patchouli_counts(
        &mut archive,
        &mut nested,
        &target_locale,
        path,
        cancellation,
    )?;
    result.source_key_count += patchouli_source_count;
    result.translated_key_count += patchouli_translated_count;

    if keys_by_namespace.is_empty() && patchouli_source_count == 0 {
        if let Some(reason) = first_language_error {
            result.status = ModLanguageStatus::Failed { reason };
        }
        return Ok(result);
    }

    for (namespace, locale_keys) in &keys_by_namespace {
        let Some(source_keys) = select_source_keys(locale_keys) else {
            continue;
        };
        result.source_key_count += source_keys.len();
        if let Some(target_keys) = target_keys_by_namespace.get(namespace) {
            result.translated_key_count += source_keys.intersection(target_keys).count();
        }
    }

    // 空白文字だけの値はEditorでも空欄に見えるため、翻訳済みとして数えない。
    // 原文側のキーは空値でもProjectの進捗集計でentryとして数えるので、分母には残す。
    result.status = if result.translated_key_count == 0 {
        ModLanguageStatus::Untranslated
    } else {
        let coverage = result.translated_key_count as f64 / result.source_key_count as f64;
        if result.translated_key_count < result.source_key_count {
            ModLanguageStatus::Partial { coverage }
        } else {
            ModLanguageStatus::Complete
        }
    };

    Ok(result)
}

fn scan_patchouli_counts(
    root: &mut Archive<File>,
    nested: &mut [yc_archive::NestedArchive],
    target_locale: &str,
    jar_path: &Path,
    cancellation: Option<&CancellationToken>,
) -> Result<(usize, usize), AnalysisError> {
    let mut total = (0, 0);
    let mut containers = Vec::with_capacity(nested.len() + 1);
    let mut root_paths = Vec::new();
    for entry in root.list_entries() {
        ensure_optional_cancellation(cancellation)?;
        if let Some(path) = entry.normalized_path.as_deref()
            && is_patchouli_json_candidate(path)
        {
            root_paths.push(path.to_owned());
        }
    }
    let mut root_files = Vec::new();
    for path in root_paths {
        ensure_optional_cancellation(cancellation)?;
        let read = root.read_entry(&path, EntryKind::Text);
        ensure_optional_cancellation(cancellation)?;
        match read {
            Ok(bytes) => root_files.push((path, bytes)),
            Err(error) => {
                tracing::warn!(path = %jar_path.display(), archive_path = %path, %error, "lightweight scan could not read Patchouli document")
            }
        }
    }
    containers.push((String::new(), root_files));

    for candidate in nested {
        ensure_optional_cancellation(cancellation)?;
        let Some(archive) = candidate.archive.as_mut() else {
            continue;
        };
        let mut paths = Vec::new();
        for entry in archive.list_entries() {
            ensure_optional_cancellation(cancellation)?;
            if let Some(path) = entry.normalized_path.as_deref()
                && is_patchouli_json_candidate(path)
            {
                paths.push(path.to_owned());
            }
        }
        let mut files = Vec::new();
        for path in paths {
            ensure_optional_cancellation(cancellation)?;
            let read = archive.read_entry(&path, EntryKind::Text);
            ensure_optional_cancellation(cancellation)?;
            match read {
                Ok(bytes) => files.push((path, bytes)),
                Err(error) => {
                    tracing::warn!(path = %jar_path.display(), nested_path = %candidate.path, archive_path = %path, %error, "lightweight scan could not read nested Patchouli document")
                }
            }
        }
        containers.push((candidate.path.clone(), files));
    }

    for (_, owned_files) in containers {
        ensure_optional_cancellation(cancellation)?;
        let files: Vec<_> = owned_files
            .iter()
            .map(|(path, bytes)| PatchouliArchiveFile {
                path,
                bytes: bytes.as_slice(),
            })
            .collect();
        let books = discover_books(&files).books;
        ensure_optional_cancellation(cancellation)?;
        for book in books {
            ensure_optional_cancellation(cancellation)?;
            let extraction = extract_entries(&book, "en_us", target_locale);
            ensure_optional_cancellation(cancellation)?;
            total.0 += extraction.entries.len();
            total.1 += extraction
                .entries
                .iter()
                .filter(|entry| {
                    entry
                        .target_text
                        .as_deref()
                        .is_some_and(|text| !text.trim().is_empty())
                })
                .count();
        }
    }
    Ok(total)
}

fn is_patchouli_json_candidate(path: &str) -> bool {
    path.ends_with(".json")
        && (path.starts_with("assets/") || path.starts_with("data/"))
        && path.contains("/patchouli_books/")
}

fn select_source_keys(locale_keys: &BTreeMap<String, HashSet<String>>) -> Option<&HashSet<String>> {
    locale_keys.get("en_us").or_else(|| {
        locale_keys
            .iter()
            .min_by(|(left_locale, left_keys), (right_locale, right_keys)| {
                right_keys
                    .len()
                    .cmp(&left_keys.len())
                    .then_with(|| left_locale.cmp(right_locale))
            })
            .map(|(_, keys)| keys)
    })
}

fn scan_archive_limits() -> ArchiveLimits {
    // Each worker retains parsed keys while it compares locales. Bound both archive metadata and
    // expanded text per JAR, then cap worker concurrency so their peak memory stays predictable.
    ArchiveLimits {
        max_entries: MAX_SCAN_ENTRIES,
        max_path_metadata_bytes: MAX_SCAN_PATH_METADATA_BYTES,
        max_text_size_bytes: MAX_SCAN_TEXT_SIZE_BYTES,
        max_cumulative_read_bytes: MAX_SCAN_CUMULATIVE_READ_BYTES,
        ..ArchiveLimits::default()
    }
}

fn is_metadata_path(path: &str) -> bool {
    matches!(
        path,
        "META-INF/MANIFEST.MF"
            | "fabric.mod.json"
            | "quilt.mod.json"
            | "META-INF/neoforge.mods.toml"
            | "META-INF/mods.toml"
            | "mcmod.info"
            | "META-INF/jarjar/metadata.json"
    )
}

fn root_asset_namespace(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("assets/")?;
    let (namespace, _) = rest.split_once('/')?;
    (!namespace.is_empty()).then_some(namespace)
}

fn is_language_candidate(path: &str) -> bool {
    let components: Vec<_> = path.split('/').collect();
    components.len() == 4
        && components[0] == "assets"
        && components[2] == "lang"
        && components[1] != "."
        && components[1] != ".."
        && components[3]
            .rsplit_once('.')
            .is_some_and(|(_, extension)| {
                extension.eq_ignore_ascii_case("json") || extension.eq_ignore_ascii_case("lang")
            })
}

fn hash_file(
    path: &Path,
    cancellation: Option<&CancellationToken>,
) -> Result<String, AnalysisError> {
    ensure_optional_cancellation(cancellation)?;
    let mut file = File::open(path).map_err(AnalysisError::HashIo)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; HASH_BUFFER_SIZE];
    loop {
        ensure_optional_cancellation(cancellation)?;
        let read = file.read(&mut buffer).map_err(AnalysisError::HashIo)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    ensure_optional_cancellation(cancellation)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn ensure_optional_cancellation(
    cancellation: Option<&CancellationToken>,
) -> Result<(), AnalysisError> {
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        Err(AnalysisError::Cancelled)
    } else {
        Ok(())
    }
}

fn failed_result(path: &Path, reason: String) -> ModScanResult {
    failed_result_with_size(path, None, reason)
}

fn failed_result_with_size(
    path: &Path,
    file_size_bytes: Option<u64>,
    reason: String,
) -> ModScanResult {
    tracing::warn!(path = %path.display(), error = %reason, "軽量JARスキャンに失敗しました");
    ModScanResult {
        path: path.to_path_buf(),
        file_name: file_name(path),
        file_size_bytes,
        sha256: None,
        mods: Vec::new(),
        source_key_count: 0,
        translated_key_count: 0,
        status: ModLanguageStatus::Failed { reason },
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<(), AnalysisError> {
    ensure_optional_cancellation(Some(cancellation))
}
