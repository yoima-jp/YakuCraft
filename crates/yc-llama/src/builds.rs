//! 固定した llama.cpp ビルドの表。
//!
//! ビルドタグ・アセット名・サイズ・SHA-256 をコードへ埋め込むのは、供給網の安全のため。
//! ダウンロード元（GitHub Releases）は後から差し替えられる可能性があり、実行ファイルを
//! 無検証で起動すると任意コード実行につながる。ここに固定値を置くことで、
//! 上流が同じタグのアセットを差し替えても検証で必ず失敗する。
//!
//! CUDA ビルドは数百 MB あり配布に適さないため提供しない。UI 上の「GPU」は Vulkan ビルドを指す
//! （NVIDIA / AMD / Intel のいずれでも動く）。

/// 固定している llama.cpp のビルドタグ。上流の stable リリース v0.9.0 に対応する。
pub const PINNED_TAG: &str = "b12110";

/// アセットの取得元。タグとアセット名を連結して使う。
const DOWNLOAD_BASE: &str = "https://github.com/ggml-org/llama.cpp/releases/download";

/// 使うバックエンド。`Gpu` は Vulkan ビルドを指す。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Backend {
    Cpu,
    Gpu,
}

impl Backend {
    /// インストール先ディレクトリ名に使う識別子。
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Cpu => "cpu",
            Backend::Gpu => "vulkan",
        }
    }
}

/// アセットの圧縮形式。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ArchiveKind {
    Zip,
    TarGz,
}

impl ArchiveKind {
    fn as_str(self) -> &'static str {
        match self {
            ArchiveKind::Zip => "zip",
            ArchiveKind::TarGz => "tar.gz",
        }
    }
}

/// 1 つの固定ビルド。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeBuild {
    /// llama.cpp のビルドタグ（例: `b12110`）。
    pub tag: &'static str,
    /// 対象 OS / アーキテクチャの識別子（例: `windows-x86_64`）。
    pub target: &'static str,
    pub backend: Backend,
    /// GitHub Releases のアセット名。
    pub asset: &'static str,
    /// アセットのバイト数。ダウンロード直後に検証する。
    pub size: u64,
    /// アセットの SHA-256（小文字 hex）。
    pub sha256: &'static str,
    pub archive: ArchiveKind,
    /// 展開後のツリー内での `llama-server` の相対パス。
    ///
    /// 実際のアセットを展開して確認した値。Windows の zip はルート直下に実行ファイルを置くが、
    /// Linux の tar.gz は `llama-b12110/` ディレクトリを 1 段挟む。
    pub server_relative_path: &'static str,
}

impl RuntimeBuild {
    /// ダウンロード URL。GitHub は署名付きオブジェクトストレージへリダイレクトする。
    pub fn download_url(&self) -> String {
        format!("{DOWNLOAD_BASE}/{}/{}", self.tag, self.asset)
    }

    /// `<runtime_root>/b12110-<backend>` のディレクトリ名。
    pub fn install_dir_name(&self) -> String {
        format!("{}-{}", self.tag, self.backend.as_str())
    }

    /// 展開後のツリー内での `llama-server` の相対パス。
    pub fn server_relative_path(&self) -> &'static str {
        self.server_relative_path
    }

    /// アセットの拡張子を含む形式名（エラーメッセージ用）。
    pub fn archive_kind_name(&self) -> &'static str {
        self.archive.as_str()
    }
}

/// 対応しているすべての固定ビルド。
pub const RUNTIME_BUILDS: &[RuntimeBuild] = &[
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "windows-x86_64",
        backend: Backend::Cpu,
        asset: "llama-b12110-bin-win-cpu-x64.zip",
        size: 24_708_445,
        sha256: "98901e327c252875accaf2d1bf364e10e868843b0b63468baad1aa945cdc5da2",
        archive: ArchiveKind::Zip,
        server_relative_path: "llama-server.exe",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "windows-x86_64",
        backend: Backend::Gpu,
        asset: "llama-b12110-bin-win-vulkan-x64.zip",
        size: 37_661_146,
        sha256: "9732a5e48887ca54d0bfe4af4324dc85fd56cd8cda19b52df3542fa6ebe6874f",
        archive: ArchiveKind::Zip,
        server_relative_path: "llama-server.exe",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "windows-aarch64",
        backend: Backend::Cpu,
        asset: "llama-b12110-bin-win-cpu-arm64.zip",
        size: 13_197_637,
        sha256: "f8d20fb07d0b499988ced8963afcd5ded2d2cdd9983b757fbc7e51c4abd722e8",
        archive: ArchiveKind::Zip,
        server_relative_path: "llama-server.exe",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "windows-aarch64",
        backend: Backend::Gpu,
        asset: "llama-b12110-bin-win-vulkan-arm64.zip",
        size: 25_728_494,
        sha256: "2be672cf538aabfc9fcb22c21648422b01d623673f15c2742bd43a9bd1a11ce2",
        archive: ArchiveKind::Zip,
        server_relative_path: "llama-server.exe",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "linux-x86_64",
        backend: Backend::Cpu,
        asset: "llama-b12110-bin-ubuntu-x64.tar.gz",
        size: 23_051_958,
        sha256: "368227edcc229be5f9bed16a2223d79b5e41d0ac2e19bcf5825e82291c060e55",
        archive: ArchiveKind::TarGz,
        server_relative_path: "llama-b12110/llama-server",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "linux-x86_64",
        backend: Backend::Gpu,
        asset: "llama-b12110-bin-ubuntu-vulkan-x64.tar.gz",
        size: 35_858_360,
        sha256: "a5d741940956ec62e8783aee7e5a4c7c761af867d6f027c3277a51e1a3ac59b0",
        archive: ArchiveKind::TarGz,
        server_relative_path: "llama-b12110/llama-server",
    },
];

/// 現在のビルドターゲットの識別子。表の `target` と突き合わせる。
fn current_target_name() -> &'static str {
    if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
        "windows-x86_64"
    } else if cfg!(target_os = "windows") && cfg!(target_arch = "aarch64") {
        "windows-aarch64"
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        "linux-x86_64"
    } else {
        "unsupported"
    }
}

/// 実行中のプラットフォーム向けの固定ビルド。未対応ターゲットでは `None`。
pub fn current_target_build(backend: Backend) -> Option<&'static RuntimeBuild> {
    let target = current_target_name();
    RUNTIME_BUILDS
        .iter()
        .find(|build| build.target == target && build.backend == backend)
}
