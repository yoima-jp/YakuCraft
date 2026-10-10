//! JAR / ZIP を展開せずに読み込む、安全制限付きの同期 API。
//!
//! ZIP走査、Entry読み込み、SHA-256計算はblocking I/Oです。TauriなどのUI層から呼ぶ場合は
//! `spawn_blocking`相当の実行器へ委譲してください。

use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{self, Cursor, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use sha2::{Digest, Sha256};
use thiserror::Error;
use zip::{CompressionMethod, ZipArchive};

const MIB: u64 = 1024 * 1024;
const MAX_EOCD_SEARCH_BYTES: u64 = 22 + u16::MAX as u64;
const CENTRAL_HEADER_SIZE: u64 = 46;
const ZIP_LOCAL_FILE_HEADER: [u8; 4] = *b"PK\x03\x04";
const ZIP_CENTRAL_DIRECTORY_HEADER: [u8; 4] = *b"PK\x01\x02";
const ZIP_END_OF_CENTRAL_DIRECTORY: [u8; 4] = *b"PK\x05\x06";
const ZIP64_END_OF_CENTRAL_DIRECTORY: [u8; 4] = *b"PK\x06\x06";
const ZIP64_END_OF_CENTRAL_DIRECTORY_LOCATOR: [u8; 4] = *b"PK\x06\x07";
const MAX_READ_CHUNK: usize = 8 * 1024;

/// Archive読み込み時に適用する上限。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveLimits {
    /// Root JARの圧縮ファイルサイズ上限。
    pub max_archive_size_bytes: u64,
    /// RootおよびNested JARそれぞれの中央ディレクトリEntry数上限。
    pub max_entries: u64,
    /// 中央ディレクトリに含まれるEntry名の合計バイト数上限。超過時はArchiveを開かない。
    pub max_path_metadata_bytes: u64,
    /// テキストEntryの展開後サイズ上限。
    pub max_text_size_bytes: u64,
    /// テクスチャEntryの展開後サイズ上限。
    pub max_texture_size_bytes: u64,
    /// モデルJSON Entryの展開後サイズ上限。
    pub max_model_json_size_bytes: u64,
    /// Nested JAR 1件の圧縮データをRootから読み込む上限。
    pub max_nested_jar_size_bytes: u64,
    /// 許容する展開後 / 圧縮サイズの最大比率。
    pub max_compression_ratio: u32,
    /// RootとNested JARを合算した、実際に展開して読み込むバイト数上限。
    pub max_cumulative_read_bytes: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_archive_size_bytes: 1024 * MIB,
            max_entries: 200_000,
            // 最大Entry数でも平均320 byteのパスを許容しつつ、名前メタデータを有限量に抑える。
            max_path_metadata_bytes: 64 * MIB,
            max_text_size_bytes: 16 * MIB,
            max_texture_size_bytes: 64 * MIB,
            max_model_json_size_bytes: 16 * MIB,
            max_nested_jar_size_bytes: 64 * MIB,
            // 200:1は通常のテキスト圧縮に余裕を残しつつ、異常に高い展開率を持つ
            // Entryを解析対象から外すための初期値。実データに応じて呼び出し側で変更できる。
            max_compression_ratio: 200,
            // 個々のEntry上限とは別に、全Entryを順次読む場合の合計展開量も1 GiBに抑える。
            max_cumulative_read_bytes: 1024 * MIB,
        }
    }
}

/// Entryの用途に対応した展開サイズ上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// テキスト用の上限を使う。
    Text,
    /// テクスチャ用の上限を使う。
    Texture,
    /// モデルJSON用の上限を使う。
    ModelJson,
    /// 呼び出し側が指定した上限を使う。
    Custom(u64),
}

impl EntryKind {
    fn max_bytes(self, limits: &ArchiveLimits) -> u64 {
        match self {
            Self::Text => limits.max_text_size_bytes,
            Self::Texture => limits.max_texture_size_bytes,
            Self::ModelJson => limits.max_model_json_size_bytes,
            Self::Custom(limit) => limit,
        }
    }
}

/// Archive内Entryのパスを正規化できない理由。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PathSafetyError {
    /// 空のパス、または正規化後に空になるパス。
    #[error("entry path is empty")]
    Empty,
    /// ルートから始まる絶対パス。
    #[error("absolute entry paths are not allowed")]
    Absolute,
    /// Windowsドライブ指定を含むパス。
    #[error("drive-prefixed entry paths are not allowed")]
    DrivePrefix,
    /// 親ディレクトリ参照を含むパス。
    #[error("parent-directory components are not allowed")]
    ParentTraversal,
    /// バックスラッシュを含むパス。
    #[error("backslashes are not allowed in entry paths")]
    Backslash,
    /// NUL byteを含むパス。
    #[error("NUL bytes are not allowed in entry paths")]
    NulByte,
    /// UTF-8として解釈できないファイル名。
    #[error("entry path is not valid UTF-8")]
    InvalidUtf8,
}

/// Entryを解析対象から除外した理由。
#[derive(Debug, Clone, PartialEq, Error)]
pub enum EntryExclusionReason {
    /// 安全な相対パスとして扱えない。
    #[error("unsafe entry path: {0}")]
    UnsafePath(PathSafetyError),
    /// 正規化後のパスがArchive内で重複している。
    #[error("duplicate entry path")]
    DuplicatePath,
    /// 圧縮比が設定上限を超えている。
    #[error("compression ratio {ratio:.2}:1 exceeds {maximum}:1")]
    CompressionRatioExceeded { ratio: f64, maximum: u32 },
    /// 宣言サイズが読み込み用途の上限を超えている。
    #[error("declared size {declared_size} exceeds {max_bytes} bytes")]
    DeclaredSizeExceeded { declared_size: u64, max_bytes: u64 },
    /// 実際の展開サイズが読み込み用途の上限を超えている。
    #[error("actual size exceeds {max_bytes} bytes")]
    ActualSizeExceeded { max_bytes: u64 },
    /// RootとNestedを合算した実際の読み込み量が上限を超えている。
    #[error("cumulative read exceeds {max_bytes} bytes")]
    CumulativeReadLimitExceeded { max_bytes: u64 },
    /// Entryの中央ディレクトリまたはローカルヘッダーを読み取れない。
    #[error("entry metadata is unreadable: {0}")]
    UnreadableMetadata(String),
    /// Entryがディレクトリである。
    #[error("directories cannot be read as file entries")]
    IsDirectory,
    /// Nested JARとして開けない。
    #[error("nested entry is not a valid ZIP archive: {0}")]
    InvalidNestedArchive(String),
    /// Nested JAR内のEntry数が上限を超えている。
    #[error("nested archive has {count} entries; maximum is {limit}")]
    TooManyNestedEntries { count: u64, limit: u64 },
    /// Entryの展開中に読み取りエラーが発生した。
    #[error("entry read failed: {0}")]
    ReadFailed(String),
}

/// Root JARを開く際の検証エラー。
#[derive(Debug, Error)]
pub enum InvalidJarError {
    /// ファイル拡張子が`.jar`ではない。
    #[error("JAR path must have a .jar extension: {path}")]
    InvalidExtension { path: PathBuf },
    /// 圧縮ファイルサイズが上限を超えている。
    #[error("archive size {size} exceeds {limit} bytes")]
    FileTooLarge { size: u64, limit: u64 },
    /// 先頭にZIP signatureがない。
    #[error("file does not start with a ZIP signature")]
    InvalidSignature,
    /// ZIP構造を開けない。
    #[error("invalid ZIP archive: {source}")]
    InvalidZip {
        #[source]
        source: zip::result::ZipError,
    },
    /// 中央ディレクトリ上のEntry数が上限を超えている。
    #[error("archive has {count} entries; maximum is {limit}")]
    TooManyEntries { count: u64, limit: u64 },
    /// 中央ディレクトリのファイル名合計がメモリ上限を超えている。
    #[error("aggregate entry path metadata {size} exceeds {limit} bytes")]
    PathMetadataTooLarge { size: u64, limit: u64 },
    /// 中央ディレクトリが不正、または矛盾している。
    #[error("malformed ZIP central directory: {0}")]
    MalformedCentralDirectory(&'static str),
}

/// Archive open後のEntry読み込みエラー。
#[derive(Debug, Error)]
pub enum ArchiveReadError {
    /// Archiveファイルを開けない。
    #[error("could not open archive {path}: {source}")]
    OpenFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// ファイルからデータを読み取れない。
    #[error("could not read file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// 指定されたパスのEntryが存在しない。
    #[error("archive entry not found: {path}")]
    EntryNotFound { path: String },
    /// Entryが安全上またはサイズ上の理由で除外された。
    #[error("archive entry excluded: {path}: {reason}")]
    EntryExcluded {
        path: String,
        #[source]
        reason: EntryExclusionReason,
    },
    /// EntryのZIPデータを開けない。
    #[error("could not open archive entry {path}: {source}")]
    OpenEntry {
        path: String,
        #[source]
        source: zip::result::ZipError,
    },
    /// Entryの展開データを読み取れない。
    #[error("could not read archive entry {path}: {source}")]
    ReadEntry {
        path: String,
        #[source]
        source: io::Error,
    },
    /// 読み込み量カウンターのmutexがpoisonedになっている。
    #[error("archive read budget is unavailable")]
    ReadBudgetPoisoned,
}

/// Archive操作のエラー。
#[derive(Debug, Error)]
pub enum ArchiveError {
    /// Root JARの検証に失敗した。
    #[error(transparent)]
    InvalidJar(#[from] InvalidJarError),
    /// Open後のEntry読み込みに失敗した。
    #[error(transparent)]
    ArchiveRead(#[from] ArchiveReadError),
    /// Nested JARからさらにNested JARを探索しようとした。
    #[error("nested archive depth is limited to one level")]
    NestedDepthExceeded,
}

/// Nested JAR探索がキャンセルされたか、通常のArchiveエラーで失敗した。
#[derive(Debug, Error)]
pub enum NestedDiscoveryError {
    /// Archive読み込み時に発生したエラー。
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    /// 呼び出し側からキャンセルされた。
    #[error("nested archive discovery was cancelled")]
    Cancelled,
}

impl From<ArchiveReadError> for NestedDiscoveryError {
    fn from(error: ArchiveReadError) -> Self {
        Self::Archive(ArchiveError::ArchiveRead(error))
    }
}

impl From<InvalidJarError> for NestedDiscoveryError {
    fn from(error: InvalidJarError) -> Self {
        Self::Archive(ArchiveError::InvalidJar(error))
    }
}

impl NestedDiscoveryError {
    fn into_archive_error(self) -> ArchiveError {
        match self {
            Self::Archive(error) => error,
            Self::Cancelled => unreachable!("non-cancellable archive path cannot be cancelled"),
        }
    }
}

/// Archive Entryのメタデータ。
#[derive(Debug, Clone, PartialEq)]
pub struct EntryInfo {
    /// Archive内パス。安全なものは正規化済みで、UTF-8でない名前は`None`。
    pub path: Option<Arc<str>>,
    /// 安全な相対パスに正規化した値。危険なパスの場合は`None`。
    pub normalized_path: Option<Arc<str>>,
    /// 圧縮後サイズ。ローカルヘッダーが不正な場合は`None`。
    pub compressed_size: Option<u64>,
    /// 圧縮前サイズ。ローカルヘッダーが不正な場合は`None`。
    pub uncompressed_size: Option<u64>,
    /// ZIP圧縮方式。ローカルヘッダーが不正な場合は`None`。
    pub compression_method: Option<CompressionMethod>,
    /// ディレクトリEntryかどうか。
    pub is_directory: bool,
    /// ヘッダーサイズから計算した圧縮比。
    pub compression_ratio: Option<f64>,
    /// 読み取り前に判定できた除外理由。
    pub exclusion_reasons: Vec<EntryExclusionReason>,
    zip_index: Option<usize>,
}

type EntryIndicesByPath = HashMap<Arc<str>, Vec<usize>>;

#[derive(Debug, Default)]
struct ReadBudgetState {
    total_read_bytes: u64,
    exceeded: bool,
}

/// JARまたはNested JARを表す、reader型に依存しない共通読み取りAPI。
pub struct Archive<R> {
    zip: ZipArchive<R>,
    limits: ArchiveLimits,
    entries: Vec<EntryInfo>,
    indices_by_path: EntryIndicesByPath,
    read_budget: Arc<Mutex<ReadBudgetState>>,
    depth: u8,
}

/// Nested JAR候補と、開けなかった場合の理由。
pub struct NestedArchive {
    /// Root JAR内でのNested JARパス。
    pub path: String,
    /// 開けたNested JAR。同じ`Archive`読み取りAPIを持つ。
    pub archive: Option<Archive<Cursor<Vec<u8>>>>,
    /// Nested JARを開かなかった理由。
    pub skipped_reason: Option<NestedJarSkippedReason>,
}

enum NestedArchiveOpenError {
    Cancelled,
    Skipped(NestedJarSkippedReason),
}

/// Nested JARを解析対象から外した理由。
#[derive(Debug, Clone, PartialEq, Error)]
pub enum NestedJarSkippedReason {
    /// パスが安全な相対パスではない。
    #[error("unsafe nested JAR path: {0}")]
    UnsafePath(#[from] PathSafetyError),
    /// パスがRoot JARに存在しない。
    #[error("nested JAR path was not found")]
    NotFound,
    /// パスが重複している。
    #[error("nested JAR path is duplicated")]
    DuplicatePath,
    /// Entry読み込み制限によって除外された。
    #[error("nested JAR entry was excluded: {0}")]
    EntryExcluded(#[from] EntryExclusionReason),
    /// Nested JARデータがZIPとして開けない。
    #[error("nested JAR is invalid: {0}")]
    InvalidArchive(String),
    /// Nested JAR内のEntry数が上限を超えている。
    #[error("nested JAR has {count} entries; maximum is {limit}")]
    TooManyEntries { count: u64, limit: u64 },
}

impl Archive<File> {
    /// `.jar`ファイルを既定のSafety Limitsで開く。
    ///
    /// この関数は同期I/Oを行う。UI threadから呼ばず、blocking用workerへ委譲すること。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ArchiveError> {
        Self::open_with_limits(path, ArchiveLimits::default())
    }

    /// `.jar`ファイルを呼び出し側指定のSafety Limitsで開く。
    ///
    /// ZIP signature、ファイルサイズ、中央ディレクトリ、Entry数を検証するが、Entry全体は展開しない。
    /// この関数は同期I/Oを行うため、UI threadから呼ばずblocking用workerへ委譲すること。
    pub fn open_with_limits(
        path: impl AsRef<Path>,
        limits: ArchiveLimits,
    ) -> Result<Self, ArchiveError> {
        let path = path.as_ref();
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
        {
            return Err(InvalidJarError::InvalidExtension {
                path: path.to_path_buf(),
            }
            .into());
        }

        let mut file = File::open(path).map_err(|source| ArchiveReadError::OpenFile {
            path: path.to_path_buf(),
            source,
        })?;
        let file_size = file
            .metadata()
            .map_err(|source| ArchiveReadError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?
            .len();
        if file_size > limits.max_archive_size_bytes {
            return Err(InvalidJarError::FileTooLarge {
                size: file_size,
                limit: limits.max_archive_size_bytes,
            }
            .into());
        }

        validate_zip_signature(&mut file)?;
        let end = read_end_record(&mut file, file_size)?;
        if end.entry_count > limits.max_entries {
            return Err(InvalidJarError::TooManyEntries {
                count: end.entry_count,
                limit: limits.max_entries,
            }
            .into());
        }
        let central_names = read_central_names(
            &mut file,
            &end,
            file_size,
            limits.max_path_metadata_bytes,
            &mut || Ok(()),
        )
        .map_err(NestedDiscoveryError::into_archive_error)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|source| ArchiveReadError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        let mut zip =
            ZipArchive::new(file).map_err(|source| InvalidJarError::InvalidZip { source })?;
        let (entries, indices_by_path) =
            build_entry_infos(&mut zip, central_names, &limits, &mut || Ok(()))
                .map_err(NestedDiscoveryError::into_archive_error)?;
        let excluded_entries = entries
            .iter()
            .filter(|entry| !entry.exclusion_reasons.is_empty())
            .count();
        tracing::debug!(
            entry_count = end.entry_count,
            excluded_entries,
            "opened JAR archive"
        );

        Ok(Self {
            zip,
            limits: limits.clone(),
            entries,
            indices_by_path,
            read_budget: Arc::new(Mutex::new(ReadBudgetState::default())),
            depth: 0,
        })
    }
}

impl<R: Read + Seek> Archive<R> {
    /// Archive内Entryを中央ディレクトリ順に返す。データ本体は展開しない。
    pub fn list_entries(&self) -> &[EntryInfo] {
        &self.entries
    }

    /// パス指定でEntryを読み、用途に応じたサイズ上限を適用する。
    pub fn read_entry(&mut self, path: &str, kind: EntryKind) -> Result<Vec<u8>, ArchiveError> {
        let normalized_path =
            normalize_archive_path(path).map_err(|reason| ArchiveReadError::EntryExcluded {
                path: path.to_owned(),
                reason: EntryExclusionReason::UnsafePath(reason),
            })?;
        let indices = self
            .indices_by_path
            .get(normalized_path.as_str())
            .ok_or_else(|| ArchiveReadError::EntryNotFound {
                path: path.to_owned(),
            })?;
        if indices.len() != 1 {
            return Err(ArchiveReadError::EntryExcluded {
                path: path.to_owned(),
                reason: EntryExclusionReason::DuplicatePath,
            }
            .into());
        }

        let info = self.entries[indices[0]].clone();
        if let Some(reason) = info.exclusion_reasons.first() {
            return Err(ArchiveReadError::EntryExcluded {
                path: path.to_owned(),
                reason: reason.clone(),
            }
            .into());
        }
        let zip_index = info
            .zip_index
            .ok_or_else(|| ArchiveReadError::EntryExcluded {
                path: path.to_owned(),
                reason: EntryExclusionReason::UnreadableMetadata(
                    "ZIP reader index is unavailable".to_owned(),
                ),
            })?;
        self.read_index(zip_index, path, kind.max_bytes(&self.limits), &info)
    }

    /// 永続readerから単一Entryを読む。累積予算はこの呼び出しだけに適用する。
    pub fn read_entry_independent(
        &mut self,
        path: &str,
        kind: EntryKind,
    ) -> Result<Vec<u8>, ArchiveError> {
        // プレビューの再読込回数で解析用の共有予算を消費しない。
        // 元の予算を復元するため、Nestedを含む解析の累積制限には影響しない。
        // read_entryを通すことでサイズ・圧縮率・パスの検証は同じまま維持する。
        let analysis_budget = std::mem::replace(
            &mut self.read_budget,
            Arc::new(Mutex::new(ReadBudgetState::default())),
        );
        let result = self.read_entry(path, kind);
        self.read_budget = analysis_budget;
        result
    }

    /// RootとNested JARを合算した、実際に展開して読み込んだバイト数を返す。
    pub fn cumulative_read_bytes(&self) -> Result<u64, ArchiveError> {
        self.read_budget
            .lock()
            .map(|state| state.total_read_bytes)
            .map_err(|_| ArchiveReadError::ReadBudgetPoisoned.into())
    }

    /// JAR署名に使われるファイルのパスを返す。
    pub fn signature_files(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let path = entry.normalized_path.as_deref()?;
                let display_path = entry.path.as_deref()?;
                let basename = path.strip_prefix("META-INF/")?;
                if basename.is_empty() || basename.contains('/') || entry.is_directory {
                    return None;
                }
                let extension = Path::new(basename).extension()?.to_str()?;
                ["SF", "RSA", "DSA", "EC"]
                    .iter()
                    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
                    .then(|| display_path.to_owned())
            })
            .collect()
    }

    /// JARが署名用ファイルを含むかどうかを返す。
    pub fn is_signed(&self) -> bool {
        self.entries.iter().any(|entry| {
            let Some(path) = entry.normalized_path.as_deref() else {
                return false;
            };
            let Some(basename) = path.strip_prefix("META-INF/") else {
                return false;
            };
            if basename.is_empty() || basename.contains('/') || entry.is_directory {
                return false;
            }
            Path::new(basename)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    ["SF", "RSA", "DSA", "EC"]
                        .iter()
                        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
                })
        })
    }

    /// Jar-in-Jar形式のNested JARを検出してメモリ上で開く。
    ///
    /// 自動検出対象は`META-INF/jars/*.jar`と`META-INF/jarjar/*.jar`。
    /// 追加パスはmetadata parserなどから受け取ったRoot JAR内パスとして扱う。
    /// Nested JARは深さ1段のみで、Rootと共有する読み込み量上限を使う。
    pub fn discover_nested(
        &mut self,
        additional_paths: &[String],
    ) -> Result<Vec<NestedArchive>, ArchiveError> {
        self.discover_nested_with_cancellation(additional_paths, || false)
            .map_err(NestedDiscoveryError::into_archive_error)
    }

    /// Nested JARを開く処理中に、呼び出し側のキャンセル状態を確認する。
    ///
    /// 大きなNested JARの展開やEntry一覧の作成中にもcallbackを呼び、
    /// Rootの累積読み込み予算を使い切る前に処理を中断できるようにする。
    pub fn discover_nested_with_cancellation(
        &mut self,
        additional_paths: &[String],
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<Vec<NestedArchive>, NestedDiscoveryError> {
        if self.depth != 0 {
            return Err(ArchiveError::NestedDepthExceeded.into());
        }
        check_cancelled(&mut is_cancelled)?;

        let mut candidates = Vec::new();
        for entry in &self.entries {
            check_cancelled(&mut is_cancelled)?;
            let (Some(normalized_path), Some(display_path)) =
                (entry.normalized_path.as_deref(), entry.path.as_deref())
            else {
                continue;
            };
            if is_conventional_nested_path(normalized_path) {
                candidates.push((display_path.to_owned(), Ok(normalized_path.to_owned())));
            }
        }
        for path in additional_paths {
            check_cancelled(&mut is_cancelled)?;
            candidates.push((path.clone(), normalize_archive_path(path)));
        }

        let mut seen_paths = BTreeMap::<String, ()>::new();
        let mut nested_archives = Vec::new();
        for (path, normalized_path) in candidates {
            check_cancelled(&mut is_cancelled)?;
            let normalized_path = match normalized_path {
                Ok(path) => path,
                Err(reason) => {
                    nested_archives.push(NestedArchive {
                        path,
                        archive: None,
                        skipped_reason: Some(NestedJarSkippedReason::UnsafePath(reason)),
                    });
                    continue;
                }
            };
            if seen_paths.insert(normalized_path.clone(), ()).is_some() {
                continue;
            }

            let Some(indices) = self.indices_by_path.get(normalized_path.as_str()) else {
                nested_archives.push(NestedArchive {
                    path,
                    archive: None,
                    skipped_reason: Some(NestedJarSkippedReason::NotFound),
                });
                continue;
            };
            if indices.len() != 1 {
                nested_archives.push(NestedArchive {
                    path,
                    archive: None,
                    skipped_reason: Some(NestedJarSkippedReason::DuplicatePath),
                });
                continue;
            }

            let info = self.entries[indices[0]].clone();
            if let Some(reason) = info.exclusion_reasons.first() {
                nested_archives.push(NestedArchive {
                    path,
                    archive: None,
                    skipped_reason: Some(NestedJarSkippedReason::EntryExcluded(reason.clone())),
                });
                continue;
            }
            let Some(zip_index) = info.zip_index else {
                nested_archives.push(NestedArchive {
                    path,
                    archive: None,
                    skipped_reason: Some(NestedJarSkippedReason::EntryExcluded(
                        EntryExclusionReason::UnreadableMetadata(
                            "ZIP reader index is unavailable".to_owned(),
                        ),
                    )),
                });
                continue;
            };
            if info.is_directory {
                nested_archives.push(NestedArchive {
                    path,
                    archive: None,
                    skipped_reason: Some(NestedJarSkippedReason::EntryExcluded(
                        EntryExclusionReason::IsDirectory,
                    )),
                });
                continue;
            }

            let bytes = match self.read_index_with_cancellation(
                zip_index,
                &path,
                self.limits.max_nested_jar_size_bytes,
                &info,
                &mut is_cancelled,
            ) {
                Ok(bytes) => bytes,
                Err(NestedDiscoveryError::Cancelled) => {
                    return Err(NestedDiscoveryError::Cancelled);
                }
                Err(NestedDiscoveryError::Archive(error)) => {
                    nested_archives.push(NestedArchive {
                        path,
                        archive: None,
                        skipped_reason: Some(nested_skip_reason(error)),
                    });
                    continue;
                }
            };
            match Self::from_nested_bytes(
                bytes,
                self.limits.clone(),
                self.read_budget.clone(),
                &mut is_cancelled,
            ) {
                Ok(archive) => nested_archives.push(NestedArchive {
                    path,
                    archive: Some(archive),
                    skipped_reason: None,
                }),
                Err(NestedArchiveOpenError::Cancelled) => {
                    return Err(NestedDiscoveryError::Cancelled);
                }
                Err(NestedArchiveOpenError::Skipped(reason)) => {
                    nested_archives.push(NestedArchive {
                        path,
                        archive: None,
                        skipped_reason: Some(reason),
                    })
                }
            }
        }
        check_cancelled(&mut is_cancelled)?;
        Ok(nested_archives)
    }

    fn read_index(
        &mut self,
        zip_index: usize,
        path: &str,
        max_bytes: u64,
        info: &EntryInfo,
    ) -> Result<Vec<u8>, ArchiveError> {
        self.read_index_with_cancellation(zip_index, path, max_bytes, info, &mut || false)
            .map_err(NestedDiscoveryError::into_archive_error)
    }

    fn read_index_with_cancellation(
        &mut self,
        zip_index: usize,
        path: &str,
        max_bytes: u64,
        info: &EntryInfo,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Vec<u8>, NestedDiscoveryError> {
        check_cancelled(is_cancelled)?;
        if info.is_directory {
            return Err(ArchiveReadError::EntryExcluded {
                path: path.to_owned(),
                reason: EntryExclusionReason::IsDirectory,
            }
            .into());
        }
        if let (Some(compressed_size), Some(uncompressed_size)) =
            (info.compressed_size, info.uncompressed_size)
        {
            let ratio = compression_ratio(uncompressed_size, compressed_size);
            if ratio > f64::from(self.limits.max_compression_ratio) {
                return Err(ArchiveReadError::EntryExcluded {
                    path: path.to_owned(),
                    reason: EntryExclusionReason::CompressionRatioExceeded {
                        ratio,
                        maximum: self.limits.max_compression_ratio,
                    },
                }
                .into());
            }
            if uncompressed_size > max_bytes {
                return Err(ArchiveReadError::EntryExcluded {
                    path: path.to_owned(),
                    reason: EntryExclusionReason::DeclaredSizeExceeded {
                        declared_size: uncompressed_size,
                        max_bytes,
                    },
                }
                .into());
            }
        }

        let mut entry =
            self.zip
                .by_index(zip_index)
                .map_err(|source| ArchiveReadError::OpenEntry {
                    path: path.to_owned(),
                    source,
                })?;
        let mut result = Vec::new();
        let mut buffer = [0_u8; MAX_READ_CHUNK];
        let mut actual_size = 0_u64;

        loop {
            check_cancelled(is_cancelled)?;
            let read_count = {
                let mut budget = self
                    .read_budget
                    .lock()
                    .map_err(|_| ArchiveReadError::ReadBudgetPoisoned)?;
                if budget.exceeded {
                    return Err(ArchiveReadError::EntryExcluded {
                        path: path.to_owned(),
                        reason: EntryExclusionReason::CumulativeReadLimitExceeded {
                            max_bytes: self.limits.max_cumulative_read_bytes,
                        },
                    }
                    .into());
                }

                let remaining_cumulative = self
                    .limits
                    .max_cumulative_read_bytes
                    .saturating_sub(budget.total_read_bytes);
                let remaining_entry = max_bytes.saturating_sub(actual_size);
                let request_size = remaining_cumulative
                    .min(remaining_entry)
                    .min((buffer.len() - 1) as u64) as usize
                    + 1;
                let read_count = entry.read(&mut buffer[..request_size]).map_err(|source| {
                    ArchiveReadError::ReadEntry {
                        path: path.to_owned(),
                        source,
                    }
                })?;
                budget.total_read_bytes = budget.total_read_bytes.saturating_add(read_count as u64);
                if budget.total_read_bytes > self.limits.max_cumulative_read_bytes {
                    budget.exceeded = true;
                }
                read_count
            };

            if read_count == 0 {
                break;
            }
            if actual_size.saturating_add(read_count as u64) > max_bytes {
                return Err(ArchiveReadError::EntryExcluded {
                    path: path.to_owned(),
                    reason: EntryExclusionReason::ActualSizeExceeded { max_bytes },
                }
                .into());
            }
            let total_read = self
                .read_budget
                .lock()
                .map_err(|_| ArchiveReadError::ReadBudgetPoisoned)?
                .total_read_bytes;
            if total_read > self.limits.max_cumulative_read_bytes {
                return Err(ArchiveReadError::EntryExcluded {
                    path: path.to_owned(),
                    reason: EntryExclusionReason::CumulativeReadLimitExceeded {
                        max_bytes: self.limits.max_cumulative_read_bytes,
                    },
                }
                .into());
            }

            actual_size += read_count as u64;
            result.extend_from_slice(&buffer[..read_count]);
        }

        if let Some(compressed_size) = info.compressed_size {
            let ratio = compression_ratio(actual_size, compressed_size);
            if ratio > f64::from(self.limits.max_compression_ratio) {
                return Err(ArchiveReadError::EntryExcluded {
                    path: path.to_owned(),
                    reason: EntryExclusionReason::CompressionRatioExceeded {
                        ratio,
                        maximum: self.limits.max_compression_ratio,
                    },
                }
                .into());
            }
        }
        Ok(result)
    }

    fn from_nested_bytes(
        bytes: Vec<u8>,
        limits: ArchiveLimits,
        read_budget: Arc<Mutex<ReadBudgetState>>,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Archive<Cursor<Vec<u8>>>, NestedArchiveOpenError> {
        let file_size = bytes.len() as u64;
        let mut cursor = Cursor::new(bytes);
        let mut check = || check_cancelled(is_cancelled);
        check().map_err(|_| NestedArchiveOpenError::Cancelled)?;
        validate_zip_signature(&mut cursor).map_err(|error| {
            NestedArchiveOpenError::Skipped(NestedJarSkippedReason::InvalidArchive(
                error.to_string(),
            ))
        })?;
        let end = read_end_record(&mut cursor, file_size).map_err(|error| {
            NestedArchiveOpenError::Skipped(NestedJarSkippedReason::InvalidArchive(
                error.to_string(),
            ))
        })?;
        if end.entry_count > limits.max_entries {
            return Err(NestedArchiveOpenError::Skipped(
                NestedJarSkippedReason::TooManyEntries {
                    count: end.entry_count,
                    limit: limits.max_entries,
                },
            ));
        }
        let central_names = read_central_names(
            &mut cursor,
            &end,
            file_size,
            limits.max_path_metadata_bytes,
            &mut check,
        )
        .map_err(|error| match error {
            NestedDiscoveryError::Cancelled => NestedArchiveOpenError::Cancelled,
            NestedDiscoveryError::Archive(error) => NestedArchiveOpenError::Skipped(
                NestedJarSkippedReason::InvalidArchive(error.to_string()),
            ),
        })?;
        check().map_err(|_| NestedArchiveOpenError::Cancelled)?;
        cursor.set_position(0);
        let mut zip = ZipArchive::new(cursor).map_err(|error| {
            NestedArchiveOpenError::Skipped(NestedJarSkippedReason::InvalidArchive(
                error.to_string(),
            ))
        })?;
        check().map_err(|_| NestedArchiveOpenError::Cancelled)?;
        let (entries, indices_by_path) =
            build_entry_infos(&mut zip, central_names, &limits, &mut check)
                .map_err(|_| NestedArchiveOpenError::Cancelled)?;
        Ok(Archive {
            zip,
            limits,
            entries,
            indices_by_path,
            read_budget,
            depth: 1,
        })
    }
}

/// Archive内パスを正規化し、安全でない形式を拒否する。
pub fn normalize_archive_path(path: &str) -> Result<String, PathSafetyError> {
    if path.is_empty() {
        return Err(PathSafetyError::Empty);
    }
    if path.contains('\0') {
        return Err(PathSafetyError::NulByte);
    }
    if path.contains('\\') {
        return Err(PathSafetyError::Backslash);
    }
    if path.starts_with('/') {
        return Err(PathSafetyError::Absolute);
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(PathSafetyError::DrivePrefix);
    }

    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(PathSafetyError::ParentTraversal),
            value => components.push(value),
        }
    }
    if components.is_empty() {
        return Err(PathSafetyError::Empty);
    }

    let mut normalized = components.join("/");
    if path.ends_with('/') {
        normalized.push('/');
    }
    Ok(normalized)
}

/// ファイル全体をメモリへ載せず、SHA-256 digestを計算する。
pub fn sha256_file(path: impl AsRef<Path>) -> Result<[u8; 32], ArchiveError> {
    let path = path.as_ref();
    let mut file = File::open(path).map_err(|source| ArchiveReadError::OpenFile {
        path: path.to_path_buf(),
        source,
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| ArchiveReadError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().into())
}

#[derive(Debug)]
struct EndRecord {
    entry_count: u64,
    central_directory_offset: u64,
    central_directory_size: u64,
}

fn validate_zip_signature<R: Read + Seek>(reader: &mut R) -> Result<(), ArchiveError> {
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|source| ArchiveReadError::ReadFile {
            path: PathBuf::from("<reader>"),
            source,
        })?;
    let mut signature = [0_u8; 4];
    let bytes_read = reader
        .read(&mut signature)
        .map_err(|source| ArchiveReadError::ReadFile {
            path: PathBuf::from("<reader>"),
            source,
        })?;
    let valid = bytes_read == signature.len()
        && (signature == ZIP_LOCAL_FILE_HEADER
            || signature == ZIP_END_OF_CENTRAL_DIRECTORY
            || signature == ZIP_CENTRAL_DIRECTORY_HEADER
            || signature == ZIP64_END_OF_CENTRAL_DIRECTORY
            || signature == ZIP64_END_OF_CENTRAL_DIRECTORY_LOCATOR);
    if !valid {
        return Err(InvalidJarError::InvalidSignature.into());
    }
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|source| ArchiveReadError::ReadFile {
            path: PathBuf::from("<reader>"),
            source,
        })?;
    Ok(())
}

fn read_end_record<R: Read + Seek>(
    reader: &mut R,
    file_size: u64,
) -> Result<EndRecord, InvalidJarError> {
    if file_size < 22 {
        return Err(InvalidJarError::MalformedCentralDirectory(
            "end record is missing",
        ));
    }
    let tail_size = file_size.min(MAX_EOCD_SEARCH_BYTES) as usize;
    reader
        .seek(SeekFrom::End(-(tail_size as i64)))
        .map_err(|_| InvalidJarError::MalformedCentralDirectory("cannot seek to end record"))?;
    let mut tail = vec![0_u8; tail_size];
    reader
        .read_exact(&mut tail)
        .map_err(|_| InvalidJarError::MalformedCentralDirectory("cannot read end record"))?;

    let end_position = (0..=tail.len() - 22)
        .rev()
        .find(|position| {
            tail.get(*position..*position + 4) == Some(&ZIP_END_OF_CENTRAL_DIRECTORY)
                && *position
                    + 22
                    + u16::from_le_bytes([tail[*position + 20], tail[*position + 21]]) as usize
                    == tail.len()
        })
        .ok_or(InvalidJarError::MalformedCentralDirectory(
            "end record signature is missing",
        ))?;
    let absolute_end_position = file_size - tail_size as u64 + end_position as u64;
    let end = &tail[end_position..end_position + 22];
    let entries_on_disk = u16::from_le_bytes([end[8], end[9]]);
    let entries_total = u16::from_le_bytes([end[10], end[11]]);
    let central_size_32 = u32::from_le_bytes([end[12], end[13], end[14], end[15]]);
    let central_offset_32 = u32::from_le_bytes([end[16], end[17], end[18], end[19]]);

    if entries_on_disk == u16::MAX
        || entries_total == u16::MAX
        || central_size_32 == u32::MAX
        || central_offset_32 == u32::MAX
    {
        if absolute_end_position < 20 {
            return Err(InvalidJarError::MalformedCentralDirectory(
                "ZIP64 locator is missing",
            ));
        }
        reader
            .seek(SeekFrom::Start(absolute_end_position - 20))
            .map_err(|_| {
                InvalidJarError::MalformedCentralDirectory("cannot seek to ZIP64 locator")
            })?;
        let mut locator = [0_u8; 20];
        reader
            .read_exact(&mut locator)
            .map_err(|_| InvalidJarError::MalformedCentralDirectory("cannot read ZIP64 locator"))?;
        if locator[..4] != ZIP64_END_OF_CENTRAL_DIRECTORY_LOCATOR {
            return Err(InvalidJarError::MalformedCentralDirectory(
                "ZIP64 locator signature is missing",
            ));
        }
        let zip64_offset = u64::from_le_bytes([
            locator[8],
            locator[9],
            locator[10],
            locator[11],
            locator[12],
            locator[13],
            locator[14],
            locator[15],
        ]);
        if zip64_offset
            .checked_add(56)
            .is_none_or(|end| end > file_size)
        {
            return Err(InvalidJarError::MalformedCentralDirectory(
                "ZIP64 end record is outside the file",
            ));
        }
        reader.seek(SeekFrom::Start(zip64_offset)).map_err(|_| {
            InvalidJarError::MalformedCentralDirectory("cannot seek to ZIP64 end record")
        })?;
        let mut zip64_end = [0_u8; 56];
        reader.read_exact(&mut zip64_end).map_err(|_| {
            InvalidJarError::MalformedCentralDirectory("cannot read ZIP64 end record")
        })?;
        if zip64_end[..4] != ZIP64_END_OF_CENTRAL_DIRECTORY {
            return Err(InvalidJarError::MalformedCentralDirectory(
                "ZIP64 end record signature is missing",
            ));
        }
        let record_size = u64::from_le_bytes([
            zip64_end[4],
            zip64_end[5],
            zip64_end[6],
            zip64_end[7],
            zip64_end[8],
            zip64_end[9],
            zip64_end[10],
            zip64_end[11],
        ]);
        if record_size < 44 {
            return Err(InvalidJarError::MalformedCentralDirectory(
                "ZIP64 end record is too short",
            ));
        }
        let entry_count = u64::from_le_bytes([
            zip64_end[32],
            zip64_end[33],
            zip64_end[34],
            zip64_end[35],
            zip64_end[36],
            zip64_end[37],
            zip64_end[38],
            zip64_end[39],
        ]);
        let central_directory_size = u64::from_le_bytes([
            zip64_end[40],
            zip64_end[41],
            zip64_end[42],
            zip64_end[43],
            zip64_end[44],
            zip64_end[45],
            zip64_end[46],
            zip64_end[47],
        ]);
        let central_directory_offset = u64::from_le_bytes([
            zip64_end[48],
            zip64_end[49],
            zip64_end[50],
            zip64_end[51],
            zip64_end[52],
            zip64_end[53],
            zip64_end[54],
            zip64_end[55],
        ]);
        validate_central_directory_bounds(
            entry_count,
            central_directory_offset,
            central_directory_size,
            file_size,
        )?;
        return Ok(EndRecord {
            entry_count,
            central_directory_offset,
            central_directory_size,
        });
    }

    if entries_on_disk != entries_total {
        return Err(InvalidJarError::MalformedCentralDirectory(
            "multi-disk ZIP archives are not supported",
        ));
    }
    let end_record = EndRecord {
        entry_count: u64::from(entries_total),
        central_directory_offset: u64::from(central_offset_32),
        central_directory_size: u64::from(central_size_32),
    };
    validate_central_directory_bounds(
        end_record.entry_count,
        end_record.central_directory_offset,
        end_record.central_directory_size,
        file_size,
    )?;
    Ok(end_record)
}

fn validate_central_directory_bounds(
    entry_count: u64,
    central_offset: u64,
    central_size: u64,
    file_size: u64,
) -> Result<(), InvalidJarError> {
    if entry_count > central_size / CENTRAL_HEADER_SIZE
        || central_offset
            .checked_add(central_size)
            .is_none_or(|end| end > file_size)
    {
        return Err(InvalidJarError::MalformedCentralDirectory(
            "entry count or central directory bounds are inconsistent",
        ));
    }
    Ok(())
}

fn read_central_names<R: Read + Seek>(
    reader: &mut R,
    end: &EndRecord,
    file_size: u64,
    max_path_metadata_bytes: u64,
    check_cancelled: &mut impl FnMut() -> Result<(), NestedDiscoveryError>,
) -> Result<Vec<Vec<u8>>, NestedDiscoveryError> {
    let central_end = end
        .central_directory_offset
        .checked_add(end.central_directory_size)
        .filter(|central_end| *central_end <= file_size)
        .ok_or(InvalidJarError::MalformedCentralDirectory(
            "central directory bounds are invalid",
        ))?;
    reader
        .seek(SeekFrom::Start(end.central_directory_offset))
        .map_err(|_| {
            InvalidJarError::MalformedCentralDirectory("cannot seek to central directory")
        })?;
    let capacity = usize::try_from(end.entry_count).map_err(|_| {
        InvalidJarError::MalformedCentralDirectory("entry count does not fit in memory")
    })?;
    let mut names = Vec::with_capacity(capacity);
    let mut retained_path_bytes = 0_u64;
    let mut fixed = [0_u8; CENTRAL_HEADER_SIZE as usize];
    for _ in 0..end.entry_count {
        check_cancelled()?;
        reader.read_exact(&mut fixed).map_err(|_| {
            InvalidJarError::MalformedCentralDirectory("central directory header is truncated")
        })?;
        if fixed[..4] != ZIP_CENTRAL_DIRECTORY_HEADER {
            return Err(InvalidJarError::MalformedCentralDirectory(
                "central directory signature is invalid",
            )
            .into());
        }
        let name_size = u16::from_le_bytes([fixed[28], fixed[29]]) as u64;
        let extra_size = u16::from_le_bytes([fixed[30], fixed[31]]) as u64;
        let comment_size = u16::from_le_bytes([fixed[32], fixed[33]]) as u64;
        let record_end = reader
            .stream_position()
            .ok()
            .and_then(|position| {
                position
                    .checked_add(name_size)
                    .and_then(|position| position.checked_add(extra_size))
                    .and_then(|position| position.checked_add(comment_size))
            })
            .filter(|record_end| *record_end <= central_end)
            .ok_or(InvalidJarError::MalformedCentralDirectory(
                "central directory entry exceeds its bounds",
            ))?;
        let next_path_bytes = retained_path_bytes.saturating_add(name_size);
        if next_path_bytes > max_path_metadata_bytes {
            return Err(InvalidJarError::PathMetadataTooLarge {
                size: next_path_bytes,
                limit: max_path_metadata_bytes,
            }
            .into());
        }
        retained_path_bytes = next_path_bytes;
        let name_len = usize::try_from(name_size).expect("ZIP filename length fits in u16");
        let mut name = vec![0_u8; name_len];
        reader.read_exact(&mut name).map_err(|_| {
            InvalidJarError::MalformedCentralDirectory("central directory filename is truncated")
        })?;
        reader.seek(SeekFrom::Start(record_end)).map_err(|_| {
            InvalidJarError::MalformedCentralDirectory("cannot skip central directory data")
        })?;
        names.push(name);
    }
    Ok(names)
}

fn build_entry_infos<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    central_names: Vec<Vec<u8>>,
    limits: &ArchiveLimits,
    check_cancelled: &mut impl FnMut() -> Result<(), NestedDiscoveryError>,
) -> Result<(Vec<EntryInfo>, EntryIndicesByPath), NestedDiscoveryError> {
    let mut zip_indices_by_path = HashMap::with_capacity(zip.len());
    for zip_index in 0..zip.len() {
        check_cancelled()?;
        if let Some(path) = zip.name_for_index(zip_index) {
            let key = normalize_archive_path(path).unwrap_or_else(|_| path.to_owned());
            zip_indices_by_path.insert(Arc::<str>::from(key), zip_index);
        }
    }

    let mut entries = Vec::with_capacity(central_names.len());
    let mut indices_by_path = EntryIndicesByPath::with_capacity(central_names.len());
    for raw_name in central_names {
        check_cancelled()?;
        let mut exclusion_reasons = Vec::new();
        let (path, normalized_path, zip_index) = match String::from_utf8(raw_name) {
            Ok(raw_path) => match normalize_archive_path(&raw_path) {
                Ok(normalized) => {
                    let (path, zip_index) = zip_indices_by_path
                        .get_key_value(normalized.as_str())
                        .map(|(path, index)| (Arc::clone(path), Some(*index)))
                        .unwrap_or_else(|| (Arc::<str>::from(normalized), None));
                    (Some(Arc::clone(&path)), Some(path), zip_index)
                }
                Err(reason) => {
                    exclusion_reasons.push(EntryExclusionReason::UnsafePath(reason));
                    let (path, zip_index) = zip_indices_by_path
                        .get_key_value(raw_path.as_str())
                        .map(|(path, index)| (Arc::clone(path), Some(*index)))
                        .unwrap_or_else(|| (Arc::<str>::from(raw_path), None));
                    (Some(path), None, zip_index)
                }
            },
            Err(_) => {
                exclusion_reasons.push(EntryExclusionReason::UnsafePath(
                    PathSafetyError::InvalidUtf8,
                ));
                (None, None, None)
            }
        };

        if let Some(normalized_path) = &normalized_path {
            let index = entries.len();
            indices_by_path
                .entry(Arc::clone(normalized_path))
                .or_default()
                .push(index);
        }
        let mut compressed_size = None;
        let mut uncompressed_size = None;
        let mut compression_method = None;
        let mut is_directory = path.as_deref().is_some_and(|path| path.ends_with('/'));
        let mut compression_ratio_value = None;
        if let Some(zip_index) = zip_index {
            match zip.by_index(zip_index) {
                Ok(file) => {
                    compressed_size = Some(file.compressed_size());
                    uncompressed_size = Some(file.size());
                    compression_method = Some(file.compression());
                    is_directory = file.is_dir();
                    let ratio = compression_ratio(file.size(), file.compressed_size());
                    compression_ratio_value = Some(ratio);
                    if ratio > f64::from(limits.max_compression_ratio) {
                        exclusion_reasons.push(EntryExclusionReason::CompressionRatioExceeded {
                            ratio,
                            maximum: limits.max_compression_ratio,
                        });
                    }
                }
                Err(error) => exclusion_reasons
                    .push(EntryExclusionReason::UnreadableMetadata(error.to_string())),
            }
        } else if path.is_some() && exclusion_reasons.is_empty() {
            exclusion_reasons.push(EntryExclusionReason::UnreadableMetadata(
                "entry is unavailable in the ZIP reader index".to_owned(),
            ));
        }

        entries.push(EntryInfo {
            path,
            normalized_path,
            compressed_size,
            uncompressed_size,
            compression_method,
            is_directory,
            compression_ratio: compression_ratio_value,
            exclusion_reasons,
            zip_index,
        });
    }

    for indices in indices_by_path.values().filter(|indices| indices.len() > 1) {
        check_cancelled()?;
        for index in indices {
            check_cancelled()?;
            entries[*index]
                .exclusion_reasons
                .push(EntryExclusionReason::DuplicatePath);
        }
    }

    Ok((entries, indices_by_path))
}

fn check_cancelled(is_cancelled: &mut impl FnMut() -> bool) -> Result<(), NestedDiscoveryError> {
    if is_cancelled() {
        Err(NestedDiscoveryError::Cancelled)
    } else {
        Ok(())
    }
}

fn compression_ratio(uncompressed_size: u64, compressed_size: u64) -> f64 {
    match (uncompressed_size, compressed_size) {
        (0, 0) => 1.0,
        (_, 0) => f64::INFINITY,
        (uncompressed_size, compressed_size) => uncompressed_size as f64 / compressed_size as f64,
    }
}

fn is_conventional_nested_path(path: &str) -> bool {
    ["META-INF/jars/", "META-INF/jarjar/"].iter().any(|prefix| {
        path.strip_prefix(prefix).is_some_and(|basename| {
            !basename.is_empty()
                && !basename.contains('/')
                && Path::new(basename)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
        })
    })
}

fn nested_skip_reason(error: ArchiveError) -> NestedJarSkippedReason {
    match error {
        ArchiveError::ArchiveRead(ArchiveReadError::EntryExcluded { reason, .. }) => {
            NestedJarSkippedReason::EntryExcluded(reason)
        }
        ArchiveError::ArchiveRead(other) => NestedJarSkippedReason::EntryExcluded(
            EntryExclusionReason::ReadFailed(other.to_string()),
        ),
        ArchiveError::InvalidJar(error) => {
            NestedJarSkippedReason::InvalidArchive(error.to_string())
        }
        ArchiveError::NestedDepthExceeded => {
            NestedJarSkippedReason::InvalidArchive("nested archive depth exceeds one".to_owned())
        }
    }
}

#[cfg(test)]
mod tests;
