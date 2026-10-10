//! JARから翻訳Reviewに必要な情報を解析する同期パイプライン。
//!
//! ZIP走査、ハッシュ計算、言語ファイル解析はblocking処理です。TauriなどのUI層から呼ぶ場合は
//! `spawn_blocking`へ委譲してください。解析結果はメモリ上に保持し、DB保存は行いません。

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::File,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use yc_archive::{
    Archive, ArchiveError, ArchiveLimits, EntryExclusionReason, EntryKind, NestedArchive,
    NestedDiscoveryError,
};
use yc_language::{
    DiscoveredLanguageFile, LanguageArchivePath, LanguageFileFormat, LanguageParseWarning,
    ParsedLanguageFile, ReviewSettings, TranslationEntry, TranslationOrigin,
    discover_language_files, merge_languages, normalize_locale,
};
use yc_metadata::{ArchiveLookup, MetadataAnalysis, ModInfo, analyze_metadata};

pub mod scan;
pub use scan::{
    ModLanguageStatus, ModScanResult, ScanOptions, ScanProgress, scan_mod_jar, scan_mods_folder,
};

const HASH_BUFFER_SIZE: usize = 64 * 1024;
const PROGRESS_ENTRY_INTERVAL: u64 = 128;
const PROGRESS_TIME_INTERVAL: Duration = Duration::from_millis(100);

/// 解析を中断するための共有トークン。
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// 解析を中断するよう要求する。
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// 中断が要求されているかを返す。
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// UIへ送る解析段階。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisStage {
    ValidateJar,
    Sha256,
    ScanZipPaths,
    ParseMetadata,
    ScanNestedJars,
    DiscoverNamespaces,
    ParseLanguages,
    DetectTargetTranslation,
}

/// 解析の進捗。Stage開始・完了時と、件数または時間の閾値を超えた時に通知する。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisProgress {
    pub stage: AnalysisStage,
    pub current: u64,
    pub total: Option<u64>,
    pub message: String,
}

/// Modのasset namespaceと、それを提供するArchive。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamespaceInfo {
    pub namespace: String,
    /// Root JARは空文字、Nested JARはRoot内のパス。
    pub container_path: String,
    /// Nested JAR由来namespaceは初期状態で翻訳対象外。
    pub included: bool,
}

/// 解析した言語ファイルの概要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageFileInfo {
    pub namespace: String,
    pub locale: String,
    pub format: LanguageFileFormat,
    pub entry_count: usize,
    pub container_path: String,
    pub archive_path: String,
}

/// Nested JARの解析結果。個別の失敗は`skipped_reason`とwarningに残す。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedArchiveInfo {
    pub archive_path: String,
    pub mod_id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub has_languages: bool,
    pub skipped_reason: Option<String>,
}

/// Fatalでない解析上の問題。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisWarning {
    pub container_path: String,
    pub path: String,
    pub message: String,
}

/// Tauri Channelなどへ送れる、解析結果の軽量DTO。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisResult {
    pub source_sha256: String,
    pub mods: Vec<ModInfo>,
    pub namespaces: Vec<NamespaceInfo>,
    pub nested_archives: Vec<NestedArchiveInfo>,
    pub language_files: Vec<LanguageFileInfo>,
    pub recommended_source_locale: Option<String>,
    pub existing_japanese_translation_count: usize,
    pub missing_source_keys_count: usize,
    pub warnings: Vec<AnalysisWarning>,
    pub is_signed: bool,
}

/// 解析結果と、Review設定の変更後も再利用するparse済み言語データ。
///
/// `result`だけをChannelへ返し、この値をRust側で保持すれば、Reviewの設定変更でJARを再解析せずに済む。
#[derive(Debug)]
pub struct AnalysisData {
    pub result: AnalysisResult,
    parsed_languages: Vec<ParsedLanguageFile>,
    default_settings: ReviewSettings,
}

impl AnalysisData {
    /// 解析直後に使うReview設定。Root namespaceを対象にし、Nestedは未選択にする。
    ///
    /// 選択数0件も有効な利用者選択として扱うため、未指定時の初期設定はこの関数で取得する。
    pub fn default_review_settings(&self) -> ReviewSettings {
        self.default_settings.clone()
    }

    /// Review設定に基づいて翻訳Entryを作り、表示用件数を再計算する。
    ///
    /// `included_namespaces`は明示選択として扱い、空ならEntryも0件になる。
    /// 初期値が必要な場合は`default_review_settings`の戻り値を渡す。
    ///
    /// Entry数に比例するblocking処理なので、UI層から使う場合もworkerへ委譲する。
    pub fn merge_review(&self, settings: &ReviewSettings) -> ReviewMergeResult {
        let settings = self.resolve_settings(settings);
        let entries = merge_languages(&self.parsed_languages, &settings);
        let existing_japanese_translation_count = entries
            .iter()
            .filter(|entry| {
                entry.origin == TranslationOrigin::Existing && !entry.target_text.trim().is_empty()
            })
            .count();
        let missing_source_keys_count = count_missing_source_keys(
            &self.parsed_languages,
            &settings.included_namespaces,
            &settings.source_locale,
        );

        ReviewMergeResult {
            entries,
            existing_japanese_translation_count,
            missing_source_keys_count,
        }
    }

    fn resolve_settings(&self, settings: &ReviewSettings) -> ReviewSettings {
        let mut resolved = settings.clone();
        if resolved.source_locale.trim().is_empty() {
            resolved.source_locale = self
                .result
                .recommended_source_locale
                .clone()
                .unwrap_or_default();
        }
        resolved
    }
}

/// Review用merge結果。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewMergeResult {
    pub entries: Vec<TranslationEntry>,
    pub existing_japanese_translation_count: usize,
    pub missing_source_keys_count: usize,
}

/// JAR解析で致命的だったエラー。
#[derive(Debug, Error)]
pub enum AnalysisError {
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error("JARのSHA-256を読み取れません: {0}")]
    HashIo(#[source] std::io::Error),
    #[error("言語ファイルが見つかりません")]
    NoLanguageFiles,
    #[error("modsフォルダを読み取れません: {path}: {source}")]
    ScanFolderIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("解析はキャンセルされました")]
    Cancelled,
}

/// JARを検証し、メタデータと言語ファイルをメモリ上へ解析する。
///
/// この同期関数はZIP I/O、ハッシュ、parseを実行するため、UI threadから直接呼ばず、
/// Tauri commandなどでは`spawn_blocking`へ委譲する。
pub fn analyze_jar(
    path: impl AsRef<Path>,
    cancellation: &CancellationToken,
    on_progress: impl FnMut(AnalysisProgress),
) -> Result<AnalysisData, AnalysisError> {
    analyze_jar_with_limits(path, ArchiveLimits::default(), cancellation, on_progress)
}

fn analyze_jar_with_limits(
    path: impl AsRef<Path>,
    limits: ArchiveLimits,
    cancellation: &CancellationToken,
    on_progress: impl FnMut(AnalysisProgress),
) -> Result<AnalysisData, AnalysisError> {
    let path = path.as_ref();
    let mut progress = ProgressReporter::new(on_progress);
    ensure_not_cancelled(cancellation)?;

    progress.begin(AnalysisStage::ValidateJar, Some(1), "JARを検証しています");
    ensure_not_cancelled(cancellation)?;
    let mut root = Archive::open_with_limits(path, limits.clone())?;
    let is_signed = root.is_signed();
    progress.finish(AnalysisStage::ValidateJar, 1, Some(1), "JARを検証しました");

    let sha256 = hash_source(path, cancellation, &mut progress)?;

    let mut warnings = Vec::new();
    progress.begin(
        AnalysisStage::ScanZipPaths,
        Some(root.list_entries().len() as u64),
        "ZIP内のパスを調べています",
    );
    let root_paths = scan_paths(
        &root,
        "",
        cancellation,
        &mut progress,
        Some(root.list_entries().len() as u64),
        &mut warnings,
    )?;

    progress.begin(
        AnalysisStage::ParseMetadata,
        Some(1),
        "Mod情報を解析しています",
    );
    ensure_not_cancelled(cancellation)?;
    let metadata_paths = paths_for_metadata(&root_paths, cancellation)?;
    let root_metadata = {
        let lookup = ArchiveLookupAdapter::new(&mut root, metadata_paths);
        analyze_metadata(&lookup)
    };
    ensure_not_cancelled(cancellation)?;
    add_metadata_warnings(&mut warnings, "", &root_metadata);
    add_loader_warnings(&mut warnings, &root_metadata);
    progress.finish(
        AnalysisStage::ParseMetadata,
        1,
        Some(1),
        "Mod情報を解析しました",
    );

    progress.begin(AnalysisStage::ScanNestedJars, None, "同梱JARを調べています");
    ensure_not_cancelled(cancellation)?;
    let mut opened_nested = Vec::new();
    let mut nested_info = Vec::new();
    // RootとNestedで累積展開量を共有し、Nested展開中のキャンセルも止めるため、archive APIへ状態を渡す。
    let mut candidates = root
        .discover_nested_with_cancellation(&root_metadata.nested_jars, || {
            cancellation.is_cancelled()
        })
        .map_err(|error| match error {
            NestedDiscoveryError::Cancelled => AnalysisError::Cancelled,
            NestedDiscoveryError::Archive(error) => AnalysisError::Archive(error),
        })?;
    candidates.sort_by(|left, right| left.path.cmp(&right.path));
    let mut nested_work = 0u64;
    for candidate in candidates {
        ensure_not_cancelled(cancellation)?;
        nested_work += 1;
        progress.advance(
            AnalysisStage::ScanNestedJars,
            nested_work,
            None,
            "同梱JARを調べています",
        );
        match inspect_nested_archive(candidate, cancellation, &mut progress, &mut nested_work)? {
            InspectedNested::Opened(opened) => {
                add_metadata_warnings(&mut warnings, &opened.info.archive_path, &opened.metadata);
                nested_info.push(opened.info.clone());
                opened_nested.push(*opened);
            }
            InspectedNested::Skipped(info) => {
                if let Some(reason) = info.skipped_reason.as_deref() {
                    warnings.push(AnalysisWarning {
                        container_path: String::new(),
                        path: info.archive_path.clone(),
                        message: format!("同梱JARを解析しませんでした: {reason}"),
                    });
                }
                nested_info.push(info);
            }
        }
    }
    progress.finish(
        AnalysisStage::ScanNestedJars,
        nested_work,
        Some(nested_work),
        "同梱JARを調べました",
    );

    progress.begin(
        AnalysisStage::DiscoverNamespaces,
        None,
        "namespaceを調べています",
    );
    ensure_not_cancelled(cancellation)?;
    let namespaces =
        discover_namespaces(&root_metadata, &opened_nested, cancellation, &mut progress)?;
    progress.finish(
        AnalysisStage::DiscoverNamespaces,
        namespaces.len() as u64,
        Some(namespaces.len() as u64),
        "namespaceを調べました",
    );

    ensure_not_cancelled(cancellation)?;
    let language_paths = collect_language_paths(&root_paths, &opened_nested, cancellation)?;
    progress.begin(
        AnalysisStage::ParseLanguages,
        Some(language_paths.len() as u64),
        "言語ファイルを解析しています",
    );
    ensure_not_cancelled(cancellation)?;
    if language_paths.is_empty() {
        progress.finish(
            AnalysisStage::ParseLanguages,
            0,
            Some(0),
            "言語ファイルがありません",
        );
        return Err(AnalysisError::NoLanguageFiles);
    }
    for info in &mut nested_info {
        info.has_languages = language_paths
            .iter()
            .any(|file| file.container_path == info.archive_path);
    }

    let mut parsed_languages = Vec::new();
    let mut language_summaries = Vec::with_capacity(language_paths.len());
    let total_languages = language_paths.len() as u64;
    let nested_indices: HashMap<String, usize> = opened_nested
        .iter()
        .enumerate()
        .map(|(index, nested)| (nested.info.archive_path.clone(), index))
        .collect();

    for (index, file) in language_paths.into_iter().enumerate() {
        ensure_not_cancelled(cancellation)?;
        let bytes = read_language_file(
            &mut root,
            &mut opened_nested,
            &nested_indices,
            &file,
            cancellation,
        );
        let entry_count = match bytes {
            Ok(bytes) => match yc_language::parse_language_file(file.clone(), &bytes) {
                Ok(parsed) => {
                    add_language_parse_warnings(&mut warnings, &parsed);
                    let count = parsed.entries.len();
                    parsed_languages.push(parsed);
                    count
                }
                Err(error) => {
                    warnings.push(AnalysisWarning {
                        container_path: file.container_path.clone(),
                        path: file.archive_path.clone(),
                        message: format!("言語ファイルを解析できませんでした: {error}"),
                    });
                    0
                }
            },
            Err(error) => {
                warnings.push(AnalysisWarning {
                    container_path: file.container_path.clone(),
                    path: file.archive_path.clone(),
                    message: format!("言語ファイルを読み取れませんでした: {error}"),
                });
                0
            }
        };
        language_summaries.push(LanguageFileInfo {
            namespace: file.namespace,
            locale: file.locale,
            format: file.format,
            entry_count,
            container_path: file.container_path,
            archive_path: file.archive_path,
        });
        progress.advance(
            AnalysisStage::ParseLanguages,
            (index + 1) as u64,
            Some(total_languages),
            "言語ファイルを解析しています",
        );
    }
    progress.finish(
        AnalysisStage::ParseLanguages,
        language_summaries.len() as u64,
        Some(language_summaries.len() as u64),
        "言語ファイルを解析しました",
    );

    // metadataから見つかるnamespaceにはiconやmodelだけのものも含まれるため、
    // 翻訳Entryを持つnamespaceだけをReviewとExportの対象として残す。
    let namespaces: Vec<_> = namespaces
        .into_iter()
        .filter(|namespace| {
            language_summaries.iter().any(|language| {
                language.namespace == namespace.namespace && language.entry_count > 0
            })
        })
        .collect();

    progress.begin(
        AnalysisStage::DetectTargetTranslation,
        Some(3),
        "翻訳元と言語の件数を調べています",
    );
    ensure_not_cancelled(cancellation)?;
    let mut default_settings = ReviewSettings {
        source_locale: String::new(),
        fallback_priority: Vec::new(),
        preserve_existing_japanese: true,
        fill_missing_from_other_languages: true,
        included_namespaces: namespaces
            .iter()
            .filter(|namespace| namespace.included)
            .map(|namespace| namespace.namespace.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    };
    let mut recommendation_namespaces = default_settings.included_namespaces.clone();
    if recommendation_namespaces.is_empty() {
        recommendation_namespaces = namespaces
            .iter()
            .map(|namespace| namespace.namespace.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    let recommendation = source_language_recommendation_cancellable(
        &parsed_languages,
        &recommendation_namespaces,
        cancellation,
    )?;
    ensure_not_cancelled(cancellation)?;
    progress.advance(
        AnalysisStage::DetectTargetTranslation,
        1,
        Some(3),
        "翻訳元を推奨しました",
    );
    default_settings.source_locale = recommendation.clone().unwrap_or_default();
    let existing_japanese_translation_count = count_existing_japanese_cancellable(
        &parsed_languages,
        &default_settings.included_namespaces,
        cancellation,
    )?;
    progress.advance(
        AnalysisStage::DetectTargetTranslation,
        2,
        Some(3),
        "既存の日本語訳を数えました",
    );
    let missing_source_keys_count = count_missing_source_keys_cancellable(
        &parsed_languages,
        &default_settings.included_namespaces,
        &default_settings.source_locale,
        cancellation,
    )?;
    ensure_not_cancelled(cancellation)?;
    progress.advance(
        AnalysisStage::DetectTargetTranslation,
        3,
        Some(3),
        "翻訳元にないキーを数えました",
    );
    progress.finish(
        AnalysisStage::DetectTargetTranslation,
        3,
        Some(3),
        "翻訳元と言語の件数を調べました",
    );

    Ok(AnalysisData {
        result: AnalysisResult {
            source_sha256: sha256,
            mods: root_metadata.mods,
            namespaces,
            nested_archives: nested_info,
            language_files: language_summaries,
            recommended_source_locale: recommendation,
            existing_japanese_translation_count,
            missing_source_keys_count,
            warnings,
            is_signed,
        },
        parsed_languages,
        default_settings,
    })
}

struct ProgressReporter<'a> {
    callback: Box<dyn FnMut(AnalysisProgress) + 'a>,
    last_current: u64,
    last_sent_at: Instant,
}

impl<'a> ProgressReporter<'a> {
    fn new(callback: impl FnMut(AnalysisProgress) + 'a) -> Self {
        Self {
            callback: Box::new(callback),
            last_current: 0,
            last_sent_at: Instant::now(),
        }
    }

    fn begin(&mut self, stage: AnalysisStage, total: Option<u64>, message: &str) {
        self.last_current = 0;
        self.emit(stage, 0, total, message);
    }

    fn advance(&mut self, stage: AnalysisStage, current: u64, total: Option<u64>, message: &str) {
        let enough_entries = current.saturating_sub(self.last_current) >= PROGRESS_ENTRY_INTERVAL;
        let enough_time = self.last_sent_at.elapsed() >= PROGRESS_TIME_INTERVAL;
        if self.last_current == 0 || enough_entries || enough_time {
            self.last_current = current;
            self.emit(stage, current, total, message);
        }
    }

    fn finish(&mut self, stage: AnalysisStage, current: u64, total: Option<u64>, message: &str) {
        self.emit(stage, current, total, message);
    }

    fn emit(&mut self, stage: AnalysisStage, current: u64, total: Option<u64>, message: &str) {
        (self.callback)(AnalysisProgress {
            stage,
            current,
            total,
            message: message.to_owned(),
        });
        self.last_sent_at = Instant::now();
    }
}

struct ArchiveLookupAdapter<'a, R> {
    archive: Mutex<&'a mut Archive<R>>,
    paths: Vec<String>,
}

impl<'a, R> ArchiveLookupAdapter<'a, R> {
    fn new(archive: &'a mut Archive<R>, paths: Vec<String>) -> Self {
        Self {
            archive: Mutex::new(archive),
            paths,
        }
    }
}

impl<R: Read + std::io::Seek> ArchiveLookup for ArchiveLookupAdapter<'_, R> {
    fn paths(&self) -> Vec<String> {
        self.paths.clone()
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let Ok(mut archive) = self.archive.lock() else {
            tracing::error!(path, "metadata lookupのarchive lockが利用できません");
            return None;
        };
        match archive.read_entry(path, EntryKind::Text) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                tracing::warn!(path, error = %error, "metadataをarchiveから読み取れません");
                None
            }
        }
    }
}

struct NestedLookupAdapter<'a> {
    archive: Mutex<&'a mut Archive<Cursor<Vec<u8>>>>,
    paths: Vec<String>,
    cancellation: &'a CancellationToken,
}

impl<'a> NestedLookupAdapter<'a> {
    fn new(
        archive: &'a mut Archive<Cursor<Vec<u8>>>,
        paths: Vec<String>,
        cancellation: &'a CancellationToken,
    ) -> Self {
        Self {
            archive: Mutex::new(archive),
            paths,
            cancellation,
        }
    }
}

impl ArchiveLookup for NestedLookupAdapter<'_> {
    fn paths(&self) -> Vec<String> {
        self.paths.clone()
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let Ok(mut archive) = self.archive.lock() else {
            tracing::error!(path, "Nested metadata lookupのarchive lockが利用できません");
            return None;
        };
        ensure_not_cancelled(self.cancellation).ok()?;
        match archive.read_entry(path, EntryKind::Text) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                tracing::warn!(path, error = %error, "Nested metadataをarchiveから読み取れません");
                None
            }
        }
    }
}

struct OpenNestedArchive {
    info: NestedArchiveInfo,
    archive: Archive<Cursor<Vec<u8>>>,
    paths: Vec<String>,
    metadata: MetadataAnalysis,
}

enum InspectedNested {
    Opened(Box<OpenNestedArchive>),
    Skipped(NestedArchiveInfo),
}

fn skipped_nested(archive_path: String, reason: impl Into<String>) -> InspectedNested {
    InspectedNested::Skipped(NestedArchiveInfo {
        archive_path,
        mod_id: None,
        name: None,
        version: None,
        has_languages: false,
        skipped_reason: Some(reason.into()),
    })
}

fn inspect_nested_archive(
    candidate: NestedArchive,
    cancellation: &CancellationToken,
    progress: &mut ProgressReporter<'_>,
    work_current: &mut u64,
) -> Result<InspectedNested, AnalysisError> {
    ensure_not_cancelled(cancellation)?;
    let archive_path = candidate.path;
    let Some(mut archive) = candidate.archive else {
        return Ok(skipped_nested(
            archive_path,
            candidate.skipped_reason.map_or_else(
                || "Nested JARを開けませんでした".to_owned(),
                |reason| reason.to_string(),
            ),
        ));
    };
    let mut paths = Vec::new();
    for entry in archive.list_entries() {
        ensure_not_cancelled(cancellation)?;
        if let Some(path) = entry.normalized_path.as_deref() {
            paths.push(path.to_owned());
        }
        *work_current += 1;
        progress.advance(
            AnalysisStage::ScanNestedJars,
            *work_current,
            None,
            &format!("同梱JAR内を調べています: {archive_path}"),
        );
    }
    paths.sort();
    paths.dedup();
    ensure_not_cancelled(cancellation)?;

    let metadata_paths = paths_for_metadata(&paths, cancellation)?;
    let metadata = {
        let lookup = NestedLookupAdapter::new(&mut archive, metadata_paths, cancellation);
        analyze_metadata(&lookup)
    };
    ensure_not_cancelled(cancellation)?;
    let mod_info = metadata.mods.first();
    Ok(InspectedNested::Opened(Box::new(OpenNestedArchive {
        info: NestedArchiveInfo {
            archive_path,
            mod_id: mod_info.and_then(|info| info.mod_id.clone()),
            name: mod_info.and_then(|info| info.name.clone()),
            version: mod_info.and_then(|info| info.version.clone()),
            has_languages: false,
            skipped_reason: None,
        },
        paths,
        archive,
        metadata,
    })))
}

fn scan_paths<R: Read + std::io::Seek>(
    archive: &Archive<R>,
    container_path: &str,
    cancellation: &CancellationToken,
    progress: &mut ProgressReporter<'_>,
    total: Option<u64>,
    warnings: &mut Vec<AnalysisWarning>,
) -> Result<Vec<String>, AnalysisError> {
    let entries = archive.list_entries();
    let mut paths = Vec::new();
    let mut exclusions = BTreeMap::<&'static str, (u64, String)>::new();
    for (index, entry) in entries.iter().enumerate() {
        if index % PROGRESS_ENTRY_INTERVAL as usize == 0 {
            ensure_not_cancelled(cancellation)?;
        }
        if let Some(path) = entry.normalized_path.as_deref() {
            paths.push(path.to_owned());
        }
        for reason in &entry.exclusion_reasons {
            let label = exclusion_label(reason);
            let sample = entry.path.as_deref().unwrap_or("<invalid path>").to_owned();
            let aggregate = exclusions.entry(label).or_insert((0, sample));
            aggregate.0 += 1;
        }
        progress.advance(
            AnalysisStage::ScanZipPaths,
            (index + 1) as u64,
            total,
            "ZIP内のパスを調べています",
        );
    }
    ensure_not_cancelled(cancellation)?;
    paths.sort();
    paths.dedup();
    for (label, (count, sample)) in exclusions {
        warnings.push(AnalysisWarning {
            container_path: container_path.to_owned(),
            path: sample,
            message: format!("安全制限により{count}件のEntryを除外しました（{label}）"),
        });
    }
    progress.finish(
        AnalysisStage::ScanZipPaths,
        entries.len() as u64,
        total,
        "ZIP内のパスを調べました",
    );
    Ok(paths)
}

fn paths_for_metadata(
    paths: &[String],
    cancellation: &CancellationToken,
) -> Result<Vec<String>, AnalysisError> {
    const METADATA_FILES: &[&str] = &[
        "fabric.mod.json",
        "quilt.mod.json",
        "META-INF/neoforge.mods.toml",
        "META-INF/mods.toml",
        "mcmod.info",
        "META-INF/MANIFEST.MF",
        "META-INF/jarjar/metadata.json",
    ];

    let mut selected = BTreeSet::new();
    let mut seen_namespaces = HashSet::new();
    for (index, path) in paths.iter().enumerate() {
        if index % PROGRESS_ENTRY_INTERVAL as usize == 0 {
            ensure_not_cancelled(cancellation)?;
        }
        if METADATA_FILES.contains(&path.as_str()) {
            selected.insert(path.clone());
            continue;
        }
        let Some(asset_path) = path.strip_prefix("assets/") else {
            continue;
        };
        let Some((namespace, _)) = asset_path.split_once('/') else {
            continue;
        };
        if seen_namespaces.insert(namespace.to_owned()) {
            // fallback Mod名の検出にはnamespaceごとに一つのasset pathがあればよい。
            selected.insert(path.clone());
        }
    }
    Ok(selected.into_iter().collect())
}

fn discover_namespaces(
    root_metadata: &MetadataAnalysis,
    nested: &[OpenNestedArchive],
    cancellation: &CancellationToken,
    progress: &mut ProgressReporter<'_>,
) -> Result<Vec<NamespaceInfo>, AnalysisError> {
    let mut unique_namespaces = HashSet::new();
    for namespace in &root_metadata.namespaces {
        ensure_not_cancelled(cancellation)?;
        unique_namespaces.insert(namespace.as_str());
    }
    for archive in nested {
        for namespace in &archive.metadata.namespaces {
            ensure_not_cancelled(cancellation)?;
            unique_namespaces.insert(namespace.as_str());
        }
    }
    let total = unique_namespaces.len() as u64;
    let mut seen_namespaces = HashSet::with_capacity(unique_namespaces.len());
    let mut namespaces = Vec::with_capacity(unique_namespaces.len());
    let mut current = 0u64;
    for namespace in &root_metadata.namespaces {
        ensure_not_cancelled(cancellation)?;
        if !seen_namespaces.insert(namespace.as_str()) {
            continue;
        }
        namespaces.push(NamespaceInfo {
            namespace: namespace.clone(),
            container_path: String::new(),
            included: true,
        });
        current += 1;
        progress.advance(
            AnalysisStage::DiscoverNamespaces,
            current,
            Some(total),
            "namespaceを調べています",
        );
    }
    for archive in nested {
        for namespace in &archive.metadata.namespaces {
            ensure_not_cancelled(cancellation)?;
            if !seen_namespaces.insert(namespace.as_str()) {
                continue;
            }
            namespaces.push(NamespaceInfo {
                namespace: namespace.clone(),
                container_path: archive.info.archive_path.clone(),
                included: false,
            });
            current += 1;
            progress.advance(
                AnalysisStage::DiscoverNamespaces,
                current,
                Some(total),
                "namespaceを調べています",
            );
        }
    }
    Ok(namespaces)
}

fn collect_language_paths(
    root_paths: &[String],
    nested: &[OpenNestedArchive],
    cancellation: &CancellationToken,
) -> Result<Vec<DiscoveredLanguageFile>, AnalysisError> {
    let mut all_paths = Vec::with_capacity(
        root_paths.len()
            + nested
                .iter()
                .map(|archive| archive.paths.len())
                .sum::<usize>(),
    );
    for (index, path) in root_paths.iter().enumerate() {
        if index % PROGRESS_ENTRY_INTERVAL as usize == 0 {
            ensure_not_cancelled(cancellation)?;
        }
        all_paths.push(LanguageArchivePath::root(path.clone()));
    }
    for archive in nested {
        for (index, path) in archive.paths.iter().enumerate() {
            if index % PROGRESS_ENTRY_INTERVAL as usize == 0 {
                ensure_not_cancelled(cancellation)?;
            }
            all_paths.push(LanguageArchivePath::nested(
                archive.info.archive_path.clone(),
                path.clone(),
            ));
        }
    }

    // discovery APIにキャンセルhookがないため、小分けに渡してpathが多いJARでも確認点を保つ。
    let mut discovered = Vec::new();
    for chunk in all_paths.chunks(512) {
        ensure_not_cancelled(cancellation)?;
        discovered.extend(discover_language_files(chunk));
        ensure_not_cancelled(cancellation)?;
    }
    discovered.sort_by(|left, right| {
        (
            !left.container_path.is_empty(),
            &left.container_path,
            &left.namespace,
            &left.archive_path,
            &left.locale,
        )
            .cmp(&(
                !right.container_path.is_empty(),
                &right.container_path,
                &right.namespace,
                &right.archive_path,
                &right.locale,
            ))
    });
    Ok(discovered)
}

fn read_language_file(
    root: &mut Archive<File>,
    nested: &mut [OpenNestedArchive],
    nested_indices: &HashMap<String, usize>,
    file: &DiscoveredLanguageFile,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, String> {
    ensure_not_cancelled(cancellation).map_err(|error| error.to_string())?;
    let bytes = if file.container_path.is_empty() {
        root.read_entry(&file.archive_path, EntryKind::Text)
            .map_err(|error| error.to_string())
    } else {
        let index = nested_indices
            .get(file.container_path.as_str())
            .copied()
            .ok_or_else(|| format!("Nested JARが見つかりません: {}", file.container_path))?;
        nested[index]
            .archive
            .read_entry(&file.archive_path, EntryKind::Text)
            .map_err(|error| error.to_string())
    };
    ensure_not_cancelled(cancellation).map_err(|error| error.to_string())?;
    bytes
}

fn add_metadata_warnings(
    warnings: &mut Vec<AnalysisWarning>,
    container_path: &str,
    metadata: &MetadataAnalysis,
) {
    warnings.extend(metadata.warnings.iter().map(|warning| AnalysisWarning {
        container_path: container_path.to_owned(),
        path: warning.path.clone(),
        message: warning.message.clone(),
    }));
}

fn add_loader_warnings(warnings: &mut Vec<AnalysisWarning>, metadata: &MetadataAnalysis) {
    if metadata
        .mods
        .iter()
        .all(|info| info.loader == yc_metadata::LoaderKind::Unknown)
    {
        warnings.push(AnalysisWarning {
            container_path: String::new(),
            path: String::new(),
            message: "Mod Loaderを特定できませんでした。言語ファイルの解析は続行します".to_owned(),
        });
    }
    if !metadata.mods.is_empty()
        && metadata
            .mods
            .iter()
            .all(|info| info.minecraft_version_estimate.is_none())
    {
        warnings.push(AnalysisWarning {
            container_path: String::new(),
            path: String::new(),
            message: "Minecraft versionを特定できませんでした".to_owned(),
        });
    }
}

fn add_language_parse_warnings(warnings: &mut Vec<AnalysisWarning>, file: &ParsedLanguageFile) {
    for warning in &file.warnings {
        let message = match warning {
            LanguageParseWarning::DuplicateKey { key } => {
                format!("重複キー「{key}」は最後の値を使用しました")
            }
            LanguageParseWarning::EncodingFallback { encoding } => {
                format!("UTF-8以外のため{encoding}として読み込みました")
            }
        };
        warnings.push(AnalysisWarning {
            container_path: file.file.container_path.clone(),
            path: file.file.archive_path.clone(),
            message,
        });
    }
}

fn count_missing_source_keys(
    files: &[ParsedLanguageFile],
    included_namespaces: &[String],
    source_locale: &str,
) -> usize {
    let included: HashSet<&str> = included_namespaces.iter().map(String::as_str).collect();
    let source_locale = normalize_locale(source_locale);
    let mut all_keys = BTreeMap::<String, BTreeSet<String>>::new();
    let mut source_keys = BTreeMap::<String, BTreeSet<String>>::new();
    for file in files
        .iter()
        .filter(|file| included.contains(file.file.namespace.as_str()))
    {
        let namespace_keys = all_keys.entry(file.file.namespace.clone()).or_default();
        for key in file.entries.keys() {
            namespace_keys.insert(key.clone());
            if file.file.normalized_locale == source_locale {
                source_keys
                    .entry(file.file.namespace.clone())
                    .or_default()
                    .insert(key.clone());
            }
        }
    }
    all_keys
        .into_iter()
        .map(|(namespace, keys)| {
            let source = source_keys.get(&namespace);
            keys.iter()
                .filter(|key| !source.is_some_and(|keys| keys.contains(*key)))
                .count()
        })
        .sum()
}

fn count_missing_source_keys_cancellable(
    files: &[ParsedLanguageFile],
    included_namespaces: &[String],
    source_locale: &str,
    cancellation: &CancellationToken,
) -> Result<usize, AnalysisError> {
    let included: HashSet<&str> = included_namespaces.iter().map(String::as_str).collect();
    let source_locale = normalize_locale(source_locale);
    let mut all_keys = BTreeMap::<String, BTreeSet<String>>::new();
    let mut source_keys = BTreeMap::<String, BTreeSet<String>>::new();
    for file in files
        .iter()
        .filter(|file| included.contains(file.file.namespace.as_str()))
    {
        ensure_not_cancelled(cancellation)?;
        let namespace_keys = all_keys.entry(file.file.namespace.clone()).or_default();
        for (index, key) in file.entries.keys().enumerate() {
            if index % PROGRESS_ENTRY_INTERVAL as usize == 0 {
                ensure_not_cancelled(cancellation)?;
            }
            namespace_keys.insert(key.clone());
            if file.file.normalized_locale == source_locale {
                source_keys
                    .entry(file.file.namespace.clone())
                    .or_default()
                    .insert(key.clone());
            }
        }
    }

    let mut missing_count = 0;
    for (namespace, keys) in all_keys {
        ensure_not_cancelled(cancellation)?;
        let source = source_keys.get(&namespace);
        for (index, key) in keys.iter().enumerate() {
            if index % PROGRESS_ENTRY_INTERVAL as usize == 0 {
                ensure_not_cancelled(cancellation)?;
            }
            if !source.is_some_and(|keys| keys.contains(key)) {
                missing_count += 1;
            }
        }
    }
    Ok(missing_count)
}

fn source_language_recommendation_cancellable(
    files: &[ParsedLanguageFile],
    included_namespaces: &[String],
    cancellation: &CancellationToken,
) -> Result<Option<String>, AnalysisError> {
    let included: HashSet<&str> = included_namespaces.iter().map(String::as_str).collect();
    let mut ordered_files: Vec<_> = files
        .iter()
        .filter(|file| included.contains(file.file.namespace.as_str()))
        .collect();
    ordered_files.sort_by(|left, right| {
        (
            !left.file.container_path.is_empty(),
            &left.file.container_path,
            &left.file.namespace,
            &left.file.archive_path,
            &left.file.locale,
        )
            .cmp(&(
                !right.file.container_path.is_empty(),
                &right.file.container_path,
                &right.file.namespace,
                &right.file.archive_path,
                &right.file.locale,
            ))
    });

    let mut locale_spellings = BTreeMap::<String, String>::new();
    let mut keys_by_locale = BTreeMap::<(String, String), HashSet<String>>::new();
    for file in ordered_files {
        ensure_not_cancelled(cancellation)?;
        locale_spellings
            .entry(file.file.normalized_locale.clone())
            .or_insert_with(|| file.file.locale.clone());
        let keys = keys_by_locale
            .entry((
                file.file.namespace.clone(),
                file.file.normalized_locale.clone(),
            ))
            .or_default();
        for (index, key) in file.entries.keys().enumerate() {
            if index % PROGRESS_ENTRY_INTERVAL as usize == 0 {
                ensure_not_cancelled(cancellation)?;
            }
            keys.insert(key.clone());
        }
    }

    let mut counts = BTreeMap::<String, usize>::new();
    for ((_, locale), keys) in keys_by_locale {
        ensure_not_cancelled(cancellation)?;
        *counts.entry(locale).or_default() += keys.len();
    }
    let recommendation = if let Some(english) = locale_spellings.get("en_us") {
        Some(english.clone())
    } else {
        let mut best_locale: Option<&str> = None;
        let mut best_count = 0;
        for (index, (locale, count)) in counts.iter().enumerate() {
            if index % PROGRESS_ENTRY_INTERVAL as usize == 0 {
                ensure_not_cancelled(cancellation)?;
            }
            if *count > best_count
                || (*count == best_count && best_locale.is_none_or(|best| locale.as_str() < best))
            {
                best_locale = Some(locale);
                best_count = *count;
            }
        }
        best_locale.and_then(|locale| locale_spellings.get(locale).cloned())
    };
    ensure_not_cancelled(cancellation)?;
    Ok(recommendation)
}

fn count_existing_japanese_cancellable(
    files: &[ParsedLanguageFile],
    included_namespaces: &[String],
    cancellation: &CancellationToken,
) -> Result<usize, AnalysisError> {
    let included: HashSet<&str> = included_namespaces.iter().map(String::as_str).collect();
    let mut effective = BTreeMap::<(String, String), bool>::new();
    for file in files.iter().filter(|file| {
        file.file.normalized_locale == "ja_jp" && included.contains(file.file.namespace.as_str())
    }) {
        ensure_not_cancelled(cancellation)?;
        for (index, (key, value)) in file.entries.iter().enumerate() {
            if index % PROGRESS_ENTRY_INTERVAL as usize == 0 {
                ensure_not_cancelled(cancellation)?;
            }
            // language file一覧はRoot優先順なので、同一keyがNestedにあってもRoot値を採用する。
            effective
                .entry((file.file.namespace.clone(), key.clone()))
                .or_insert_with(|| {
                    value
                        .text
                        .as_ref()
                        .is_some_and(|translation| !translation.trim().is_empty())
                });
        }
    }
    let mut translated_count = 0;
    for (index, is_translated) in effective.values().enumerate() {
        if index % PROGRESS_ENTRY_INTERVAL as usize == 0 {
            ensure_not_cancelled(cancellation)?;
        }
        if *is_translated {
            translated_count += 1;
        }
    }
    Ok(translated_count)
}

fn exclusion_label(reason: &EntryExclusionReason) -> &'static str {
    match reason {
        EntryExclusionReason::UnsafePath(_) => "unsafe_path",
        EntryExclusionReason::DuplicatePath => "duplicate_path",
        EntryExclusionReason::CompressionRatioExceeded { .. } => "compression_ratio",
        EntryExclusionReason::DeclaredSizeExceeded { .. } => "declared_size",
        EntryExclusionReason::ActualSizeExceeded { .. } => "actual_size",
        EntryExclusionReason::CumulativeReadLimitExceeded { .. } => "cumulative_read_limit",
        EntryExclusionReason::UnreadableMetadata(_) => "unreadable_metadata",
        EntryExclusionReason::IsDirectory => "directory_entry",
        EntryExclusionReason::InvalidNestedArchive(_) => "invalid_nested_archive",
        EntryExclusionReason::TooManyNestedEntries { .. } => "too_many_nested_entries",
        EntryExclusionReason::ReadFailed(_) => "read_failed",
    }
}

fn hash_source(
    path: &Path,
    cancellation: &CancellationToken,
    progress: &mut ProgressReporter<'_>,
) -> Result<String, AnalysisError> {
    ensure_not_cancelled(cancellation)?;
    let mut file = File::open(path).map_err(AnalysisError::HashIo)?;
    let total = file.metadata().map_err(AnalysisError::HashIo)?.len();
    progress.begin(
        AnalysisStage::Sha256,
        Some(total),
        "SHA-256を計算しています",
    );
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; HASH_BUFFER_SIZE];
    let mut current = 0u64;
    loop {
        ensure_not_cancelled(cancellation)?;
        let read = file.read(&mut buffer).map_err(AnalysisError::HashIo)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        current += read as u64;
        progress.advance(
            AnalysisStage::Sha256,
            current,
            Some(total),
            "SHA-256を計算しています",
        );
    }
    ensure_not_cancelled(cancellation)?;
    progress.finish(
        AnalysisStage::Sha256,
        current,
        Some(total),
        "SHA-256を計算しました",
    );
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<(), AnalysisError> {
    if cancellation.is_cancelled() {
        Err(AnalysisError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
