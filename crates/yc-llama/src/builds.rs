//! 固定した llama.cpp ビルドの表。
//!
//! ビルドタグ・アセット名・サイズ・SHA-256 をコードへ埋め込むのは、供給網の安全のため。
//! ダウンロード元（GitHub Releases）は後から差し替えられる可能性があり、実行ファイルを
//! 無検証で起動すると任意コード実行につながる。ここに固定値を置くことで、
//! 上流が同じタグのアセットを差し替えても検証で必ず失敗する。
//!
//! CUDA 版はビルド本体（`ggml-cuda.dll` / `libggml-cuda.so`）と、それが必要とする CUDA
//! ランタイム（cudart / cuBLAS）の 2 アーカイブに分かれている。両方を同じディレクトリへ
//! 展開して初めて起動できるため、[`RuntimeBuild`] は 1 つではなく複数のアーカイブを持つ。
//!
//! # CUDA の系統を選ぶ規則
//!
//! b12110 は CUDA 12.4 と 13.4 の 2 系統を配布している。どちらを選ぶかは GPU とドライバーで
//! 決まる（判定は [`crate::gpu::cuda_variant_for`]）。
//!
//! - CUDA 13.x はドライバー 580 以上が必要（NVIDIA「CUDA Toolkit Release Notes」の
//!   "CUDA Toolkit and Minimum Required Driver Version for CUDA Minor Version Compatibility"
//!   で 13.x は >= 580、12.x は >= 525）。また CUDA 13.0 で Maxwell / Pascal / Volta の
//!   オフラインコンパイルが打ち切られたため、compute capability 7.5（Turing）以上が要る。
//! - 13.x のビルドに含まれる実アーキテクチャも 7.5 以上に限られる。b12110 の
//!   `ggml/src/ggml-cuda/CMakeLists.txt` は CTK 13 では 50/61/70 の virtual arch を積まず、
//!   75-virtual 以上だけを積む（リリース手順も `CMAKE_CUDA_ARCHITECTURES` を指定せず
//!   この既定集合を使う。`.github/workflows/release.yml` の windows-cuda / ubuntu-cuda を参照）。
//! - CUDA 12.4 はドライバー 525 以上で動く（マイナーバージョン互換）。compute capability は
//!   5.0 以上を PTX で覆うが、sm_120（Blackwell / RTX 50）向けの実コードは持たないため、
//!   新しい GPU では PTX の JIT になる（動くが遅い）。
//!
//! したがって「ドライバー >= 580 かつ compute capability >= 7.5 なら 13.4、そうでなく
//! ドライバー >= 525 なら 12.4、どちらも満たさなければ CUDA は提供しない」とする。
//! compute capability が取れない古いドライバーでは 7.5 以上を確認できないため 12.4 を選ぶ。
//!
//! Windows ARM64 には CUDA 版を置かない。Windows-on-ARM で NVIDIA GPU を使う構成は
//! 実質存在せず、上流も x64 向けの cudart しか配布していないため。

/// 固定している llama.cpp のビルドタグ。上流の stable リリース v0.9.0 に対応する。
pub const PINNED_TAG: &str = "b12110";

/// アセットの取得元。タグとアセット名を連結して使う。
const DOWNLOAD_BASE: &str = "https://github.com/ggml-org/llama.cpp/releases/download";

/// CUDA ビルドの系統。同じ b12110 でも CUDA Toolkit の版が違う 2 系統が配布されている。
///
/// 系統はあくまで「12 系 / 13 系」の 2 つ。12 系の中身（Toolkit の版・実コードの範囲・
/// ドライバー下限）はプラットフォームで違う（Windows は 12.4、Linux は 12.8）ため、
/// それらは [`CudaToolchain`] としてビルドごとに持つ。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CudaVariant {
    /// CUDA 12 系でビルドしたもの。古いドライバー・古い GPU でも動く。
    Cuda12,
    /// CUDA 13 系でビルドしたもの。新しいドライバーと Turing 以降の GPU が要る。
    Cuda13,
}

impl CudaVariant {
    /// ログに出す系統名。
    pub fn as_str(self) -> &'static str {
        match self {
            CudaVariant::Cuda12 => "cuda-12",
            CudaVariant::Cuda13 => "cuda-13",
        }
    }
}

/// 実際に固定している CUDA ビルドのツールチェーン情報。
///
/// 系統（12 / 13）だけでは足りない。b12110 の CUDA 12 系は Windows が 12.4、Linux が 12.8 で、
/// Toolkit のドライバー下限も実コードの範囲も違う。これらをプラットフォーム非依存の
/// [`CudaVariant`] に置くと、片方の OS の値を他方へ流用してしまう。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CudaToolchain {
    /// 実際の Toolkit の版（例: `12.4` / `12.8` / `13.4`）。インストール先の名前に使う。
    pub toolkit_version: &'static str,
    /// このビルドが実コード（SASS）を持つ compute capability。
    pub native_architectures: &'static [(u32, u32)],
    /// このビルドが PTX を積む下限の compute capability。`None` なら PTX を積まない。
    pub ptx_floor: Option<(u32, u32)>,
    /// マイナーバージョン互換の下限ドライバー。実コードで動く GPU 向け。
    pub minor_compatibility_driver: u32,
    /// この Toolkit 自身が同梱するドライバーの下限。PTX を JIT する GPU 向け。
    pub toolkit_driver: u32,
}

impl CudaToolchain {
    /// このビルドを載せられる最小のドライバー（メジャーバージョン）。
    ///
    /// NVIDIA の CUDA Compatibility Guide のとおり、PTX は「古いドライバーでは動かない」。
    /// 実コードを持つ GPU はマイナーバージョン互換の下限で動くが、PTX に頼る GPU は
    /// **PTX を JIT できるドライバー**、すなわちその Toolkit 自身のドライバー下限が要る。
    pub fn minimum_driver_for(&self, capability: (u32, u32)) -> u32 {
        if self.native_architectures.contains(&capability) {
            return self.minor_compatibility_driver;
        }
        match self.ptx_floor {
            // PTX で動く。古いドライバーでは JIT できないので Toolkit の下限を要求する。
            Some(floor) if capability >= floor => self.toolkit_driver,
            // 実コードも PTX も無い。このビルドでは動かせない。
            _ => u32::MAX,
        }
    }
}

/// Windows x86_64 の CUDA 12.4。既定集合の `-real` は 86 / 89 だけ（120a は 12.8 以降）。
const CUDA_12_4: CudaToolchain = CudaToolchain {
    toolkit_version: "12.4",
    native_architectures: &[(8, 6), (8, 9)],
    // 50/61/70/75/80/90 を virtual（PTX）で積む。
    ptx_floor: Some((5, 0)),
    // CUDA 12.x のマイナーバージョン互換は >= 525。
    minor_compatibility_driver: 525,
    // CUDA 12.4 GA の Toolkit ドライバー: Windows >= 551.61 / Linux >= 550.54.14。
    // b12110 は 12.4 を Windows にしか配布しないので Windows の値を使う。
    toolkit_driver: 551,
};

/// Linux x86_64 の CUDA 12.8。Windows の 12.4 とは Toolkit も実コードも下限も違う。
const CUDA_12_8: CudaToolchain = CudaToolchain {
    toolkit_version: "12.8",
    // CTK 12.8 は 120a-real を積む（121a は 12.9 以降）。
    native_architectures: &[(8, 6), (8, 9), (12, 0)],
    ptx_floor: Some((5, 0)),
    minor_compatibility_driver: 525,
    // CUDA 12.8 GA の Toolkit ドライバー: Linux >= 570.26 / Windows >= 570.65。
    // このアーカイブは Linux 版なので Linux の値を使う。
    toolkit_driver: 570,
};

/// CUDA 13.4。Windows / Linux とも同じ既定集合（86 / 89 / 120a / 121a）。
const CUDA_13_4: CudaToolchain = CudaToolchain {
    toolkit_version: "13.4",
    native_architectures: &[(8, 6), (8, 9), (12, 0), (12, 1)],
    // CTK 13 は 50/61/70 を積まず、75/80/90 の PTX だけを積む。
    ptx_floor: Some((7, 5)),
    // CUDA 13.x のマイナーバージョン互換は >= 580。
    minor_compatibility_driver: 580,
    // CUDA 13.4 のドライバーブランチは R615。
    toolkit_driver: 615,
};

/// 使うバックエンド。GPU は Vulkan と CUDA の 2 系統ある。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Backend {
    Cpu,
    Vulkan,
    Cuda(CudaVariant),
}

impl Backend {
    /// インストール先ディレクトリ名に使う識別子。
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Cpu => "cpu",
            Backend::Vulkan => "vulkan",
            Backend::Cuda(variant) => variant.as_str(),
        }
    }

    /// GPU を使うバックエンドか。CPU だけが `false`。
    pub fn is_gpu(self) -> bool {
        !matches!(self, Backend::Cpu)
    }

    /// `llama-server --list-devices` が返すデバイス名の接頭辞。
    ///
    /// ビルドに含まれるバックエンドの種類ごとに `Vulkan0` / `CUDA0` のような名前で出る。
    /// CPU ビルドはデバイスを持たないので `None`。
    pub fn device_prefix(self) -> Option<&'static str> {
        match self {
            Backend::Cpu => None,
            Backend::Vulkan => Some("Vulkan"),
            Backend::Cuda(_) => Some("CUDA"),
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

/// 1 つの固定アセット。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeArchive {
    /// GitHub Releases のアセット名。
    pub asset: &'static str,
    /// アセットのバイト数。ダウンロード直後に検証する。
    pub size: u64,
    /// アセットの SHA-256（小文字 hex）。
    pub sha256: &'static str,
    pub archive: ArchiveKind,
    /// 展開時に取り除く先頭ディレクトリ。
    ///
    /// tar.gz は 1 段ディレクトリを挟むものがあり、そのままだと複数アーカイブを同じ
    /// ディレクトリへ重ねられない。実際のアセットを展開して確認した値だけを入れる。
    pub strip_prefix: Option<&'static str>,
    /// 展開先のルートからの相対ディレクトリ。`None` ならルート直下。
    ///
    /// CUDA の cudart は `llama-server` と同じディレクトリに無いと動的リンカが見つけられない
    /// （RPATH は `$ORIGIN`）。Linux 版はビルド本体が `llama-b12110/` を挟むため、cudart も
    /// その中へ置く。
    pub destination: Option<&'static str>,
}

impl RuntimeArchive {
    /// ダウンロード URL。GitHub は署名付きオブジェクトストレージへリダイレクトする。
    pub fn download_url(&self, tag: &str) -> String {
        format!("{DOWNLOAD_BASE}/{tag}/{}", self.asset)
    }

    /// アセットの拡張子を含む形式名（エラーメッセージ用）。
    pub fn archive_kind_name(&self) -> &'static str {
        self.archive.as_str()
    }
}

/// 1 つの固定ビルド。同じディレクトリへ展開する 1 つ以上のアーカイブからなる。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeBuild {
    /// llama.cpp のビルドタグ（例: `b12110`）。
    pub tag: &'static str,
    /// 対象 OS / アーキテクチャの識別子（例: `windows-x86_64`）。
    pub target: &'static str,
    pub backend: Backend,
    /// CUDA ビルドのツールチェーン情報。CUDA 以外は `None`。
    ///
    /// 系統（12 / 13）だけでは足りない。12 系は Windows が 12.4、Linux が 12.8 で、
    /// Toolkit のドライバー下限も実コードの範囲も違う。
    pub cuda: Option<CudaToolchain>,
    /// 展開するアーカイブ。CUDA はビルド本体と cudart の 2 つ。
    pub archives: &'static [RuntimeArchive],
    /// 展開後のツリー内での `llama-server` の相対パス。
    ///
    /// 実際のアセットを展開して確認した値。Windows の zip はルート直下に実行ファイルを置くが、
    /// Linux の tar.gz は `llama-b12110/` ディレクトリを 1 段挟む。
    pub server_relative_path: &'static str,
}

impl RuntimeBuild {
    /// `<runtime_root>/b12110-<backend>` のディレクトリ名。
    ///
    /// CUDA は実際の Toolkit の版まで含める（`b12110-cuda-12.8`）。Windows の 12.4 と
    /// Linux の 12.8 が同じディレクトリを共有すると、cudart が上書きし合って壊れる。
    pub fn install_dir_name(&self) -> String {
        match self.cuda {
            Some(toolchain) => format!("{}-cuda-{}", self.tag, toolchain.toolkit_version),
            None => format!("{}-{}", self.tag, self.backend.as_str()),
        }
    }

    /// 展開後のツリー内での `llama-server` の相対パス。
    pub fn server_relative_path(&self) -> &'static str {
        self.server_relative_path
    }

    /// 全アーカイブの合計バイト数。ダウンロード進捗の分母と UI の表示に使う。
    pub fn total_size(&self) -> u64 {
        self.archives.iter().map(|archive| archive.size).sum()
    }
}

/// Windows x86_64 の CUDA ビルド本体と cudart。zip はどちらもルート直下に展開される。
const WINDOWS_CUDA_12: &[RuntimeArchive] = &[
    RuntimeArchive {
        asset: "llama-b12110-bin-win-cuda-12.4-x64.zip",
        size: 287_014_657,
        sha256: "1ce755bc3a061889234048f88e33328e17971eaad73c4666cfeb26026b317fee",
        archive: ArchiveKind::Zip,
        strip_prefix: None,
        destination: None,
    },
    RuntimeArchive {
        asset: "cudart-llama-bin-win-cuda-12.4-x64.zip",
        size: 391_443_627,
        sha256: "8c79a9b226de4b3cacfd1f83d24f962d0773be79f1e7b75c6af4ded7e32ae1d6",
        archive: ArchiveKind::Zip,
        strip_prefix: None,
        destination: None,
    },
];

const WINDOWS_CUDA_13: &[RuntimeArchive] = &[
    RuntimeArchive {
        asset: "llama-b12110-bin-win-cuda-13.4-x64.zip",
        size: 169_175_655,
        sha256: "dc910a969190b5d42e189afd73117f6bbdd8dd8ed6586342c5e0b82cab62c673",
        archive: ArchiveKind::Zip,
        strip_prefix: None,
        destination: None,
    },
    RuntimeArchive {
        asset: "cudart-llama-bin-win-cuda-13.4-x64.zip",
        size: 423_535_356,
        sha256: "738f8c251ac22b70c3ae6f83a10cf222725df0395246a2cf58f32bdb85fbe668",
        archive: ArchiveKind::Zip,
        strip_prefix: None,
        destination: None,
    },
];

const LINUX_CUDA_12: &[RuntimeArchive] = &[
    RuntimeArchive {
        asset: "llama-b12110-bin-ubuntu-cuda-12.8-x64.tar.gz",
        size: 187_314_621,
        sha256: "4af1e1da23661db4a5b84b29aae16ebc053df689c5b8b1427c4670f2c6406d8c",
        archive: ArchiveKind::TarGz,
        strip_prefix: None,
        destination: None,
    },
    // cudart は `cudart-llama-b12110-bin-ubuntu-cuda-12.8-x64/` を 1 段挟む。中身は
    // libcudart / libcublas / libcublasLt の 3 つで、`llama-b12110/` に置く必要がある。
    RuntimeArchive {
        asset: "cudart-llama-b12110-bin-ubuntu-cuda-12.8-x64.tar.gz",
        size: 594_371_477,
        sha256: "6f1d562086d84ae85df880d3711079ec225602b51d530608e5001600625067dd",
        archive: ArchiveKind::TarGz,
        strip_prefix: Some("cudart-llama-b12110-bin-ubuntu-cuda-12.8-x64"),
        destination: Some("llama-b12110"),
    },
];

const LINUX_CUDA_13: &[RuntimeArchive] = &[
    RuntimeArchive {
        asset: "llama-b12110-bin-ubuntu-cuda-13.4-x64.tar.gz",
        size: 168_275_993,
        sha256: "2afd2ca540da119570b1dac36b5a2c862bb03a3b789c46661dfa55e600f5b19b",
        archive: ArchiveKind::TarGz,
        strip_prefix: None,
        destination: None,
    },
    RuntimeArchive {
        asset: "cudart-llama-b12110-bin-ubuntu-cuda-13.4-x64.tar.gz",
        size: 440_232_657,
        sha256: "9b1110161943882ec85c8e9ae641075ee8c962a50f8f1e36534ee65ea1e56ed9",
        archive: ArchiveKind::TarGz,
        strip_prefix: Some("cudart-llama-b12110-bin-ubuntu-cuda-13.4-x64"),
        destination: Some("llama-b12110"),
    },
];

/// 対応しているすべての固定ビルド。
pub const RUNTIME_BUILDS: &[RuntimeBuild] = &[
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "windows-x86_64",
        backend: Backend::Cpu,
        cuda: None,
        archives: &[RuntimeArchive {
            asset: "llama-b12110-bin-win-cpu-x64.zip",
            size: 24_708_445,
            sha256: "98901e327c252875accaf2d1bf364e10e868843b0b63468baad1aa945cdc5da2",
            archive: ArchiveKind::Zip,
            strip_prefix: None,
            destination: None,
        }],
        server_relative_path: "llama-server.exe",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "windows-x86_64",
        backend: Backend::Vulkan,
        cuda: None,
        archives: &[RuntimeArchive {
            asset: "llama-b12110-bin-win-vulkan-x64.zip",
            size: 37_661_146,
            sha256: "9732a5e48887ca54d0bfe4af4324dc85fd56cd8cda19b52df3542fa6ebe6874f",
            archive: ArchiveKind::Zip,
            strip_prefix: None,
            destination: None,
        }],
        server_relative_path: "llama-server.exe",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "windows-x86_64",
        backend: Backend::Cuda(CudaVariant::Cuda12),
        cuda: Some(CUDA_12_4),
        archives: WINDOWS_CUDA_12,
        server_relative_path: "llama-server.exe",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "windows-x86_64",
        backend: Backend::Cuda(CudaVariant::Cuda13),
        cuda: Some(CUDA_13_4),
        archives: WINDOWS_CUDA_13,
        server_relative_path: "llama-server.exe",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "windows-aarch64",
        backend: Backend::Cpu,
        cuda: None,
        archives: &[RuntimeArchive {
            asset: "llama-b12110-bin-win-cpu-arm64.zip",
            size: 13_197_637,
            sha256: "f8d20fb07d0b499988ced8963afcd5ded2d2cdd9983b757fbc7e51c4abd722e8",
            archive: ArchiveKind::Zip,
            strip_prefix: None,
            destination: None,
        }],
        server_relative_path: "llama-server.exe",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "windows-aarch64",
        backend: Backend::Vulkan,
        cuda: None,
        archives: &[RuntimeArchive {
            asset: "llama-b12110-bin-win-vulkan-arm64.zip",
            size: 25_728_494,
            sha256: "2be672cf538aabfc9fcb22c21648422b01d623673f15c2742bd43a9bd1a11ce2",
            archive: ArchiveKind::Zip,
            strip_prefix: None,
            destination: None,
        }],
        server_relative_path: "llama-server.exe",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "linux-x86_64",
        backend: Backend::Cpu,
        cuda: None,
        archives: &[RuntimeArchive {
            asset: "llama-b12110-bin-ubuntu-x64.tar.gz",
            size: 23_051_958,
            sha256: "368227edcc229be5f9bed16a2223d79b5e41d0ac2e19bcf5825e82291c060e55",
            archive: ArchiveKind::TarGz,
            strip_prefix: None,
            destination: None,
        }],
        server_relative_path: "llama-b12110/llama-server",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "linux-x86_64",
        backend: Backend::Vulkan,
        cuda: None,
        archives: &[RuntimeArchive {
            asset: "llama-b12110-bin-ubuntu-vulkan-x64.tar.gz",
            size: 35_858_360,
            sha256: "a5d741940956ec62e8783aee7e5a4c7c761af867d6f027c3277a51e1a3ac59b0",
            archive: ArchiveKind::TarGz,
            strip_prefix: None,
            destination: None,
        }],
        server_relative_path: "llama-b12110/llama-server",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "linux-x86_64",
        backend: Backend::Cuda(CudaVariant::Cuda12),
        cuda: Some(CUDA_12_8),
        archives: LINUX_CUDA_12,
        server_relative_path: "llama-b12110/llama-server",
    },
    RuntimeBuild {
        tag: PINNED_TAG,
        target: "linux-x86_64",
        backend: Backend::Cuda(CudaVariant::Cuda13),
        cuda: Some(CUDA_13_4),
        archives: LINUX_CUDA_13,
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
