// テストでは既定値から 1 項目だけ変えた設定を作る場面が多く、
// どの項目を変えたかが 1 行で読める代入形式の方が意図を追いやすいため許可する。
#![allow(clippy::field_reassign_with_default)]

use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio_util::sync::CancellationToken;

use crate::{
    builds::{
        ArchiveKind, Backend, CudaVariant, PINNED_TAG, RUNTIME_BUILDS, RuntimeArchive,
        RuntimeBuild, current_target_build,
    },
    download::{download_verified_for_test, validate_download_url},
    gpu::{NvidiaGpu, cuda_variant_for, parse_nvidia_smi_output},
    install::{ensure_runtime, installed_runtime, remove_runtime},
    manager::RuntimeManager,
    server::{ServerConfig, ServerHandle, ServerLauncher, parse_list_devices},
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// テスト用の一時ディレクトリ。drop で丸ごと消す。
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "yc-llama-test-{}-{label}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// 固定ビルド表
// ---------------------------------------------------------------------------

#[test]
fn every_build_targets_the_pinned_tag() {
    assert!(!RUNTIME_BUILDS.is_empty());
    for build in RUNTIME_BUILDS {
        assert_eq!(build.tag, PINNED_TAG, "{}", build.install_dir_name());
        assert!(
            !build.archives.is_empty(),
            "{} has no archive",
            build.install_dir_name()
        );
        for archive in build.archives {
            assert!(
                archive.sha256.len() == 64 && archive.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "{} has a malformed sha256",
                archive.asset
            );
            assert_eq!(
                archive.sha256,
                archive.sha256.to_ascii_lowercase(),
                "{} sha256 must be lowercase hex",
                archive.asset
            );
            assert!(archive.size > 0, "{} has no size", archive.asset);
        }
    }
}

#[test]
fn build_table_has_one_entry_per_supported_target_and_backend() {
    let mut targets = Vec::new();
    for build in RUNTIME_BUILDS {
        assert!(
            !targets.contains(&(build.target, build.backend)),
            "duplicate entry for {} {:?}",
            build.target,
            build.backend
        );
        targets.push((build.target, build.backend));
    }
    assert_eq!(targets.len(), 10);
    for target in ["windows-x86_64", "windows-aarch64", "linux-x86_64"] {
        for backend in [Backend::Cpu, Backend::Vulkan] {
            assert!(
                RUNTIME_BUILDS
                    .iter()
                    .any(|build| build.target == target && build.backend == backend),
                "missing {target} {backend:?}"
            );
        }
    }
    // CUDA は x64 の 2 ターゲットにだけある。Windows ARM64 は CPU / Vulkan のみ。
    for target in ["windows-x86_64", "linux-x86_64"] {
        for variant in [CudaVariant::Cuda12, CudaVariant::Cuda13] {
            assert!(
                RUNTIME_BUILDS
                    .iter()
                    .any(|build| build.target == target && build.backend == Backend::Cuda(variant)),
                "missing {target} {variant:?}"
            );
        }
    }
    assert!(
        !RUNTIME_BUILDS
            .iter()
            .any(|build| build.target == "windows-aarch64"
                && build.backend.is_gpu()
                && matches!(build.backend, Backend::Cuda(_))),
        "CUDA must not be offered on Windows ARM64"
    );
}

#[test]
fn cuda_builds_consist_of_the_llama_and_cudart_archives() {
    for build in RUNTIME_BUILDS
        .iter()
        .filter(|build| matches!(build.backend, Backend::Cuda(_)))
    {
        assert_eq!(
            build.archives.len(),
            2,
            "{} needs the llama build and the cudart archive",
            build.install_dir_name()
        );
        // 1 つは ggml-cuda を含むビルド本体、もう 1 つは cudart。
        assert!(build.archives[0].asset.contains("llama-"));
        assert!(build.archives[1].asset.starts_with("cudart-"));
        // 合計サイズは両方の和。UI のダウンロード量表示がこれを使う。
        assert_eq!(
            build.total_size(),
            build
                .archives
                .iter()
                .map(|archive| archive.size)
                .sum::<u64>()
        );
        // cudart は llama-server と同じディレクトリに置かないと動的リンカが見つけられない。
        let cudart = build.archives[1];
        if build.target == "linux-x86_64" {
            assert_eq!(cudart.destination, Some("llama-b12110"));
            assert!(cudart.strip_prefix.is_some());
        } else {
            assert_eq!(cudart.destination, None);
            assert_eq!(cudart.strip_prefix, None);
        }
    }
}

#[test]
fn build_urls_and_install_dirs_are_derived_from_the_tag() {
    let build = RUNTIME_BUILDS
        .iter()
        .find(|build| build.archives[0].asset == "llama-b12110-bin-win-cpu-x64.zip")
        .expect("windows cpu build");
    assert_eq!(
        build.archives[0].download_url(build.tag),
        "https://github.com/ggml-org/llama.cpp/releases/download/b12110/llama-b12110-bin-win-cpu-x64.zip"
    );
    assert_eq!(build.install_dir_name(), "b12110-cpu");
    assert_eq!(build.server_relative_path(), "llama-server.exe");
    assert_eq!(build.archives[0].archive_kind_name(), "zip");

    let vulkan = RUNTIME_BUILDS
        .iter()
        .find(|build| build.archives[0].asset == "llama-b12110-bin-win-vulkan-x64.zip")
        .expect("windows vulkan build");
    assert_eq!(vulkan.install_dir_name(), "b12110-vulkan");
    assert_eq!(vulkan.backend, Backend::Vulkan);

    // CUDA は系統ごとに別のディレクトリへ入る。同じディレクトリに混ぜると、
    // 12.x と 13.x の cudart が上書きし合って壊れる。
    let cuda12 = RUNTIME_BUILDS
        .iter()
        .find(|build| {
            build.backend == Backend::Cuda(CudaVariant::Cuda12) && build.target == "windows-x86_64"
        })
        .expect("windows cuda 12 build");
    assert_eq!(cuda12.install_dir_name(), "b12110-cuda-12.4");
    let cuda13 = RUNTIME_BUILDS
        .iter()
        .find(|build| {
            build.backend == Backend::Cuda(CudaVariant::Cuda13) && build.target == "windows-x86_64"
        })
        .expect("windows cuda 13 build");
    assert_eq!(cuda13.install_dir_name(), "b12110-cuda-13.4");

    let linux = RUNTIME_BUILDS
        .iter()
        .find(|build| build.target == "linux-x86_64" && build.backend == Backend::Cpu)
        .expect("linux cpu build");
    assert_eq!(linux.install_dir_name(), "b12110-cpu");
    assert_eq!(linux.archives[0].archive, ArchiveKind::TarGz);
    // Linux の tar.gz は 1 段ディレクトリを挟む。Windows の zip は挟まない。
    assert_eq!(linux.server_relative_path(), "llama-b12110/llama-server");
}

#[test]
fn current_target_build_matches_the_running_platform() {
    for backend in [Backend::Cpu, Backend::Vulkan] {
        let build = current_target_build(backend);
        if cfg!(any(
            all(target_os = "windows", target_arch = "x86_64"),
            all(target_os = "windows", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64"),
        )) {
            let build = build.expect("supported target must resolve a build");
            assert_eq!(build.backend, backend);
            if cfg!(windows) {
                assert_eq!(build.archives[0].archive, ArchiveKind::Zip);
                assert_eq!(build.server_relative_path(), "llama-server.exe");
            } else {
                assert_eq!(build.archives[0].archive, ArchiveKind::TarGz);
            }
        } else {
            assert!(build.is_none());
        }
    }
    // CUDA は Windows ARM64 では解決しない。
    if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        for variant in [CudaVariant::Cuda12, CudaVariant::Cuda13] {
            assert!(current_target_build(Backend::Cuda(variant)).is_none());
        }
    }
}

// ---------------------------------------------------------------------------
// GPU 検出
// ---------------------------------------------------------------------------

#[test]
fn nvidia_smi_output_is_parsed_into_every_gpu() {
    let gpus = parse_nvidia_smi_output("NVIDIA GeForce RTX 3060, 591.74, 8.6\n");
    assert_eq!(gpus.len(), 1);
    assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 3060");
    assert_eq!(gpus[0].driver_major, 591);
    assert_eq!(gpus[0].compute_capability, Some((8, 6)));

    // 複数 GPU はすべて保持する。llama.cpp は `--device` を指定しない限り全部を使うため、
    // 先頭だけ見ると遅い 1 台の存在を見落とす。
    let gpus = parse_nvidia_smi_output(
        "NVIDIA GeForce RTX 4090, 570.86, 8.9\nNVIDIA GeForce GTX 1050, 570.86, 6.1\n",
    );
    assert_eq!(gpus.len(), 2);
    assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 4090");
    assert_eq!(gpus[1].name, "NVIDIA GeForce GTX 1050");
    assert_eq!(gpus[1].compute_capability, Some((6, 1)));
}

#[test]
fn nvidia_smi_output_without_compute_capability_still_detects_the_gpu() {
    // `compute_cap` 列を持たない古いドライバーは 2 列だけを返す。
    let gpus = parse_nvidia_smi_output("NVIDIA GeForce GTX 980, 470.256.02\n");
    assert_eq!(gpus.len(), 1);
    assert_eq!(gpus[0].driver_major, 470);
    assert_eq!(gpus[0].compute_capability, None);

    // 列はあるが値が `[N/A]` のドライバーもある。
    let gpus = parse_nvidia_smi_output("NVIDIA GeForce RTX 3060, 591.74, [N/A]\n");
    assert_eq!(gpus.len(), 1);
    assert_eq!(gpus[0].compute_capability, None);

    // GPU が無ければ空文字が返る。
    assert!(parse_nvidia_smi_output("").is_empty());
    assert!(parse_nvidia_smi_output("No devices were found\n").is_empty());
}

#[test]
fn cuda_variant_follows_the_driver_and_compute_capability() {
    let gpu = |driver_major, capability| NvidiaGpu {
        name: "test".to_string(),
        driver_major,
        compute_capability: capability,
    };
    let single = |driver_major, capability| cuda_variant_for(&[gpu(driver_major, capability)]);

    // 実コード（86 / 89）を持つ GPU は、新しいドライバーで CUDA 13。
    assert_eq!(single(591, Some((8, 6))), Some(CudaVariant::Cuda13));
    assert_eq!(single(580, Some((8, 9))), Some(CudaVariant::Cuda13));
    // 13.x の下限を割るドライバーは 12.x。
    assert_eq!(single(579, Some((8, 6))), Some(CudaVariant::Cuda12));
    // 13.x が対象外にした古い GPU も 12.x（PTX を JIT できるドライバーが要る）。
    assert_eq!(single(591, Some((6, 1))), Some(CudaVariant::Cuda12));
    // 7.5 は CTK 13 の既定集合では virtual（PTX）だけ。実コードが無いので、PTX を JIT
    // できるドライバー（615 系）が要る。580 では CUDA 13 を提供しない。
    assert_eq!(single(580, Some((7, 5))), Some(CudaVariant::Cuda12));
    assert_eq!(single(615, Some((7, 5))), Some(CudaVariant::Cuda13));
    // compute capability が取れないと実コードの有無を判定できないので、PTX 前提で 12.x。
    assert_eq!(single(591, None), Some(CudaVariant::Cuda12));
    // 12.x の下限も割れば CUDA は提供しない。
    assert_eq!(single(524, Some((8, 6))), None);
    assert_eq!(single(470, None), None);
    // Kepler（3.x）は CUDA 12 の対象外。
    assert_eq!(single(591, Some((3, 5))), None);
    // GPU が 1 台も無ければ CUDA は提供しない。
    assert_eq!(cuda_variant_for(&[]), None);
}

/// 実コードを持たない GPU は PTX の JIT に頼る。NVIDIA の CUDA Compatibility Guide のとおり
/// PTX は古いドライバーでは動かないため、マイナーバージョン互換の下限（525）では提供しない。
///
/// 下限はプラットフォームの実際のアーカイブで違う（12 系は Windows 12.4 = 551、
/// Linux 12.8 = 570）。ここではビルドから下限を引いて、両方の OS で正しく判定されることを見る。
#[test]
fn a_ptx_only_gpu_requires_a_driver_that_can_jit_its_ptx() {
    let Some(toolchain) =
        current_target_build(Backend::Cuda(CudaVariant::Cuda12)).and_then(|build| build.cuda)
    else {
        return;
    };
    let floor = toolchain.minimum_driver_for((6, 1));
    assert_eq!(
        floor, toolchain.toolkit_driver,
        "cc 6.1 has no native code, so it must need the toolkit driver"
    );
    assert!(
        floor > toolchain.minor_compatibility_driver,
        "PTX must require more than minor-version compatibility"
    );

    let gpu = |driver_major| NvidiaGpu {
        name: "GTX 1080".to_string(),
        driver_major,
        compute_capability: Some((6, 1)),
    };

    // 下限の 1 つ手前までは提供しない。
    for driver in [toolchain.minor_compatibility_driver, floor - 2, floor - 1] {
        assert_eq!(cuda_variant_for(&[gpu(driver)]), None, "driver {driver}");
    }
    // 下限ちょうどから動く。
    for driver in [floor, floor + 1, 580] {
        assert_eq!(
            cuda_variant_for(&[gpu(driver)]),
            Some(CudaVariant::Cuda12),
            "driver {driver}"
        );
    }
}

/// CUDA 12 系は Windows が 12.4、Linux が 12.8 で、Toolkit のドライバー下限が違う。
/// 片方の値を他方へ流用すると、Linux の 551〜569 で「提供する」と誤判定してしまう。
#[test]
fn the_cuda_12_toolchain_matches_the_archive_that_is_actually_shipped() {
    let windows = RUNTIME_BUILDS
        .iter()
        .find(|build| {
            build.target == "windows-x86_64" && build.backend == Backend::Cuda(CudaVariant::Cuda12)
        })
        .and_then(|build| build.cuda)
        .expect("windows cuda 12 build");
    let linux = RUNTIME_BUILDS
        .iter()
        .find(|build| {
            build.target == "linux-x86_64" && build.backend == Backend::Cuda(CudaVariant::Cuda12)
        })
        .and_then(|build| build.cuda)
        .expect("linux cuda 12 build");

    // 実際のアセット名と Toolkit の版が一致していること。
    assert_eq!(windows.toolkit_version, "12.4");
    assert_eq!(linux.toolkit_version, "12.8");
    assert_eq!(
        windows.toolkit_driver, 551,
        "CUDA 12.4 GA Windows is >= 551.61"
    );
    assert_eq!(linux.toolkit_driver, 570, "CUDA 12.8 GA Linux is >= 570.26");
    assert!(
        linux.toolkit_driver > windows.toolkit_driver,
        "the Linux 12.8 archive must not reuse the Windows 12.4 floor"
    );

    // 12.8 は 120a の実コードを持つ（12.4 は持たない）。sm_120 は 12.8 なら実コードで動く。
    assert!(linux.native_architectures.contains(&(12, 0)));
    assert!(!windows.native_architectures.contains(&(12, 0)));
    assert_eq!(linux.minimum_driver_for((12, 0)), 525);
    assert_eq!(windows.minimum_driver_for((12, 0)), windows.toolkit_driver);

    // インストール先は実際の Toolkit の版で分かれる（同じディレクトリを共有しない）。
    let windows_build = RUNTIME_BUILDS
        .iter()
        .find(|build| {
            build.target == "windows-x86_64" && build.backend == Backend::Cuda(CudaVariant::Cuda12)
        })
        .expect("windows cuda 12 build");
    let linux_build = RUNTIME_BUILDS
        .iter()
        .find(|build| {
            build.target == "linux-x86_64" && build.backend == Backend::Cuda(CudaVariant::Cuda12)
        })
        .expect("linux cuda 12 build");
    assert_eq!(windows_build.install_dir_name(), "b12110-cuda-12.4");
    assert_eq!(linux_build.install_dir_name(), "b12110-cuda-12.8");
}

#[test]
fn cuda_variant_covers_every_exposed_gpu() {
    let gpu = |name: &str, capability| NvidiaGpu {
        name: name.to_string(),
        driver_major: 591,
        compute_capability: Some(capability),
    };

    // 1 台目が新しくても、2 台目が 7.5 未満なら 13.x は選べない。llama.cpp は `--device` を
    // 指定しない限り見えている GPU をすべて使うため、13.x の実コードが無い GPU が混ざる。
    assert_eq!(
        cuda_variant_for(&[gpu("RTX 4090", (8, 9)), gpu("GTX 1050", (6, 1))]),
        Some(CudaVariant::Cuda12)
    );
    // 13.x が対象外にした GPU が混ざれば、CUDA 12 の下限すら割るなら提供しない。
    assert_eq!(
        cuda_variant_for(&[gpu("RTX 4090", (8, 9)), gpu("GTX 780", (3, 5))]),
        None
    );
    // 両方とも実コードを持つなら 13.x。
    assert_eq!(
        cuda_variant_for(&[gpu("RTX 4090", (8, 9)), gpu("RTX 3060", (8, 6))]),
        Some(CudaVariant::Cuda13)
    );
    // 1 台でも compute capability が取れないと実コードの有無を判定できないので 12.x。
    assert_eq!(
        cuda_variant_for(&[
            gpu("RTX 4090", (8, 9)),
            NvidiaGpu {
                name: "unknown".to_string(),
                driver_major: 591,
                compute_capability: None,
            },
        ]),
        Some(CudaVariant::Cuda12)
    );
}

#[test]
fn detected_gpu_info_has_consistent_backend_selection() {
    // 実機の検出結果そのものは環境依存なので、整合性だけを見る。
    let info = crate::gpu::detect_gpu_info();
    let cuda = crate::gpu::cuda_backend();
    if let Some(backend) = cuda {
        let variant = match backend {
            Backend::Cuda(variant) => variant,
            other => panic!("cuda_backend returned {other:?}"),
        };
        assert_eq!(
            Some(variant),
            cuda_variant_for(&info.nvidia),
            "the selected variant must match the detected GPUs"
        );
    }
    if crate::gpu::vulkan_backend().is_some() {
        assert!(info.vulkan, "Vulkan must only be offered with a loader");
    }
}

// ---------------------------------------------------------------------------
// `--list-devices` の解析
// ---------------------------------------------------------------------------

#[test]
fn list_devices_output_is_parsed_into_named_devices() {
    // b12110 の Vulkan ビルドが実際に返す出力。
    let devices = parse_list_devices(
        "Available devices:\n  Vulkan0: NVIDIA GeForce RTX 3060 (12108 MiB, 11340 MiB free)\n",
    );
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "Vulkan0");
    assert_eq!(
        devices[0].description,
        "NVIDIA GeForce RTX 3060 (12108 MiB, 11340 MiB free)"
    );

    // CUDA ビルドの出力。
    let devices = parse_list_devices(
        "Available devices:\n  CUDA0: NVIDIA GeForce RTX 3060 (12287 MiB, 11238 MiB free)\n",
    );
    assert_eq!(devices[0].name, "CUDA0");

    // デバイスが無いときは `(none)` だけが返る。見出し行をデバイスと誤認しないこと。
    assert!(parse_list_devices("Available devices:\n  (none)\n").is_empty());
    assert!(parse_list_devices("").is_empty());
}

#[test]
fn backend_device_prefixes_match_the_build_kind() {
    assert_eq!(Backend::Cpu.device_prefix(), None);
    assert_eq!(Backend::Vulkan.device_prefix(), Some("Vulkan"));
    assert_eq!(
        Backend::Cuda(CudaVariant::Cuda13).device_prefix(),
        Some("CUDA")
    );
    assert!(!Backend::Cpu.is_gpu());
    assert!(Backend::Vulkan.is_gpu());
    assert!(Backend::Cuda(CudaVariant::Cuda12).is_gpu());
}

// ---------------------------------------------------------------------------
// URL ポリシー
// ---------------------------------------------------------------------------

#[test]
fn download_urls_must_use_https() {
    assert!(validate_download_url("https://example.com/a.zip").is_ok());
    assert!(
        validate_download_url("https://huggingface.co/x/resolve/main/m.gguf?download=1").is_ok()
    );

    assert!(matches!(
        validate_download_url("http://example.com/a.zip"),
        Err(crate::LlamaError::InsecureUrl)
    ));
    assert!(matches!(
        validate_download_url("ftp://example.com/a.zip"),
        Err(crate::LlamaError::InsecureUrl)
    ));
    assert!(matches!(
        validate_download_url("file:///C:/a.zip"),
        Err(crate::LlamaError::InsecureUrl)
    ));
    assert!(matches!(
        validate_download_url("not a url"),
        Err(crate::LlamaError::InsecureUrl)
    ));
}

#[test]
fn every_pinned_build_url_passes_the_policy() {
    for build in RUNTIME_BUILDS {
        for archive in build.archives {
            assert!(
                validate_download_url(&archive.download_url(build.tag)).is_ok(),
                "{} is not an acceptable download URL",
                archive.asset
            );
        }
    }
}

// ---------------------------------------------------------------------------
// ダウンロード（ローカルのテスト HTTP サーバー）
// ---------------------------------------------------------------------------

/// テスト用の小さなペイロード。サイズ・ハッシュを実データから計算する。
fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[tokio::test]
async fn download_verified_writes_the_file_and_reports_progress() {
    let body = payload(200_000);
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = TempDir::new("download-ok");
    let dest = dir.path().join("asset.bin");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);

    download_verified_for_test(
        &format!("{}/asset.bin", server.uri()),
        &dest,
        body.len() as u64,
        &sha256_hex(&body),
        move |received, total| recorder.lock().unwrap().push((received, total)),
        CancellationToken::new(),
    )
    .await
    .expect("download should succeed");

    assert_eq!(std::fs::read(&dest).expect("read dest"), body);
    assert!(!dir.path().join("asset.bin.part").exists());
    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty());
    assert_eq!(
        *seen.last().unwrap(),
        (body.len() as u64, body.len() as u64)
    );
}

#[tokio::test]
async fn download_verified_rejects_a_checksum_mismatch_and_drops_the_partial_file() {
    let body = payload(4_096);
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = TempDir::new("download-bad-hash");
    let dest = dir.path().join("asset.bin");
    let result = download_verified_for_test(
        &format!("{}/asset.bin", server.uri()),
        &dest,
        body.len() as u64,
        &"0".repeat(64),
        |_, _| {},
        CancellationToken::new(),
    )
    .await;

    assert!(matches!(result, Err(crate::LlamaError::ChecksumMismatch)));
    assert!(!dest.exists());
    assert!(
        !dir.path().join("asset.bin.part").exists(),
        "a corrupt partial file must not be kept for resume"
    );
}

#[tokio::test]
async fn download_verified_rejects_a_size_mismatch() {
    let body = payload(1_024);
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = TempDir::new("download-bad-size");
    let dest = dir.path().join("asset.bin");
    let result = download_verified_for_test(
        &format!("{}/asset.bin", server.uri()),
        &dest,
        body.len() as u64 + 1,
        &sha256_hex(&body),
        |_, _| {},
        CancellationToken::new(),
    )
    .await;

    assert!(matches!(
        result,
        Err(crate::LlamaError::SizeMismatch { .. })
    ));
    assert!(!dest.exists());
}

#[tokio::test]
async fn download_verified_resumes_from_an_existing_partial_file() {
    let body = payload(300_000);
    let prefix_len = 120_000;
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::header(
            "range",
            format!("bytes={prefix_len}-"),
        ))
        .respond_with(
            wiremock::ResponseTemplate::new(206)
                .insert_header(
                    "content-range",
                    format!("bytes {prefix_len}-{}/{}", body.len() - 1, body.len()),
                )
                .set_body_bytes(body[prefix_len..].to_vec()),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new("download-resume");
    let dest = dir.path().join("asset.bin");
    let part = dir.path().join("asset.bin.part");
    std::fs::write(&part, &body[..prefix_len]).expect("write partial");

    download_verified_for_test(
        &format!("{}/asset.bin", server.uri()),
        &dest,
        body.len() as u64,
        &sha256_hex(&body),
        |_, _| {},
        CancellationToken::new(),
    )
    .await
    .expect("resumed download should succeed");

    assert_eq!(std::fs::read(&dest).expect("read dest"), body);
}

#[tokio::test]
async fn download_verified_restarts_when_the_server_ignores_range() {
    let body = payload(150_000);
    let server = wiremock::MockServer::start().await;
    // Range を無視して 200 で全体を返すサーバ。
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = TempDir::new("download-ignore-range");
    let dest = dir.path().join("asset.bin");
    let part = dir.path().join("asset.bin.part");
    std::fs::write(&part, &body[..50_000]).expect("write partial");

    download_verified_for_test(
        &format!("{}/asset.bin", server.uri()),
        &dest,
        body.len() as u64,
        &sha256_hex(&body),
        |_, _| {},
        CancellationToken::new(),
    )
    .await
    .expect("restarted download should succeed");

    assert_eq!(std::fs::read(&dest).expect("read dest"), body);
}

#[tokio::test]
async fn download_verified_discards_a_partial_file_larger_than_expected() {
    let body = payload(10_000);
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = TempDir::new("download-oversized-part");
    let dest = dir.path().join("asset.bin");
    let part = dir.path().join("asset.bin.part");
    std::fs::write(&part, vec![0xAA; 50_000]).expect("write oversized partial");

    download_verified_for_test(
        &format!("{}/asset.bin", server.uri()),
        &dest,
        body.len() as u64,
        &sha256_hex(&body),
        |_, _| {},
        CancellationToken::new(),
    )
    .await
    .expect("download should restart from scratch");

    assert_eq!(std::fs::read(&dest).expect("read dest"), body);
}

#[tokio::test]
async fn download_verified_keeps_the_partial_file_on_cancel() {
    let body = payload(400_000);
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = TempDir::new("download-cancel");
    let dest = dir.path().join("asset.bin");
    let cancel = CancellationToken::new();
    cancel.cancel();

    let result = download_verified_for_test(
        &format!("{}/asset.bin", server.uri()),
        &dest,
        body.len() as u64,
        &sha256_hex(&body),
        |_, _| {},
        cancel,
    )
    .await;

    assert!(matches!(result, Err(crate::LlamaError::Cancelled)));
    assert!(!dest.exists());
}

#[tokio::test]
async fn download_verified_skips_the_network_when_the_file_is_already_correct() {
    let body = payload(8_192);
    // リクエストが来たら失敗させる。既存ファイルが正しければ一度も呼ばれないはず。
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let dir = TempDir::new("download-already-there");
    let dest = dir.path().join("asset.bin");
    std::fs::write(&dest, &body).expect("write existing file");

    download_verified_for_test(
        &format!("{}/asset.bin", server.uri()),
        &dest,
        body.len() as u64,
        &sha256_hex(&body),
        |_, _| {},
        CancellationToken::new(),
    )
    .await
    .expect("a matching existing file must be reused");

    assert_eq!(std::fs::read(&dest).unwrap(), body);
}

#[tokio::test]
async fn download_verified_replaces_a_corrupt_existing_file() {
    let body = payload(8_192);
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = TempDir::new("download-replace-corrupt");
    let dest = dir.path().join("asset.bin");
    // サイズは同じだが中身が違うファイル。
    let mut corrupt = body.clone();
    corrupt[0] ^= 0xFF;
    std::fs::write(&dest, &corrupt).expect("write corrupt file");

    download_verified_for_test(
        &format!("{}/asset.bin", server.uri()),
        &dest,
        body.len() as u64,
        &sha256_hex(&body),
        |_, _| {},
        CancellationToken::new(),
    )
    .await
    .expect("a corrupt existing file must be replaced");

    assert_eq!(std::fs::read(&dest).unwrap(), body);
}

#[tokio::test]
async fn download_verified_maps_http_errors_to_a_typed_status() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let dir = TempDir::new("download-404");
    let dest = dir.path().join("asset.bin");
    let result = download_verified_for_test(
        &format!("{}/asset.bin", server.uri()),
        &dest,
        10,
        &"0".repeat(64),
        |_, _| {},
        CancellationToken::new(),
    )
    .await;

    assert!(matches!(
        result,
        Err(crate::LlamaError::HttpStatus { status: 404 })
    ));
}

// ---------------------------------------------------------------------------
// 展開の安全性
// ---------------------------------------------------------------------------

fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    let file = std::fs::File::create(path).expect("create zip");
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for (name, data) in entries {
        writer.start_file(*name, options).expect("start zip entry");
        writer.write_all(data).expect("write zip entry");
    }
    writer.finish().expect("finish zip");
}

fn write_tar_gz(path: &Path, entries: &[(&str, &[u8])]) {
    use flate2::{Compression, write::GzEncoder};

    let file = std::fs::File::create(path).expect("create tar.gz");
    let encoder = GzEncoder::new(file, Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (name, data) in entries {
        // `append_data` はパスを検証して `..` を拒否してしまうため、生の name フィールドへ
        // 直接書いてから `append` する。悪意あるアーカイブを再現するための細工。
        let mut header = tar::Header::new_gnu();
        let raw = name.as_bytes();
        assert!(raw.len() < 100, "test fixture name is too long");
        header.as_old_mut().name[..raw.len()].copy_from_slice(raw);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, *data).expect("append tar entry");
    }
    builder.finish().expect("finish tar");
}

/// シンボリックリンクを含む tar.gz を書く。`(name, target)` の組を渡す。
/// Windows ではシンボリックリンクを作れないため、この fixture は cfg(unix) のテストだけが使う。
#[cfg(unix)]
fn write_tar_gz_with_symlinks(path: &Path, files: &[(&str, &[u8])], links: &[(&str, &str)]) {
    use flate2::{Compression, write::GzEncoder};

    let file = std::fs::File::create(path).expect("create tar.gz");
    let encoder = GzEncoder::new(file, Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        let raw = name.as_bytes();
        assert!(raw.len() < 100, "test fixture name is too long");
        header.as_old_mut().name[..raw.len()].copy_from_slice(raw);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, *data).expect("append tar entry");
    }
    for (name, target) in links {
        let mut header = tar::Header::new_gnu();
        let raw = name.as_bytes();
        assert!(raw.len() < 100, "test fixture name is too long");
        header.as_old_mut().name[..raw.len()].copy_from_slice(raw);
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_link_name(*target).expect("set link target");
        header.set_cksum();
        builder.append(&header, &[][..]).expect("append tar link");
    }
    builder.finish().expect("finish tar");
}

#[test]
fn extraction_rejects_zip_slip_entries() {
    let dir = TempDir::new("zip-slip");
    let archive = dir.path().join("evil.zip");
    write_zip(
        &archive,
        &[
            ("llama-server.exe", b"binary"),
            ("../../escaped.txt", b"pwned"),
        ],
    );

    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let result = crate::install::extract_archive_for_test(&archive, &staging, ArchiveKind::Zip);
    assert!(matches!(
        result,
        Err(crate::LlamaError::UnsafeArchiveEntry { .. })
    ));
    assert!(!dir.path().join("escaped.txt").exists());
    assert!(!dir.path().parent().unwrap().join("escaped.txt").exists());
}

#[test]
fn extraction_rejects_absolute_paths() {
    let dir = TempDir::new("zip-absolute");
    let archive = dir.path().join("evil.zip");
    write_zip(&archive, &[("/etc/passwd", b"pwned")]);

    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let result = crate::install::extract_archive_for_test(&archive, &staging, ArchiveKind::Zip);
    assert!(matches!(
        result,
        Err(crate::LlamaError::UnsafeArchiveEntry { .. })
    ));
}

#[test]
fn extraction_rejects_tar_slip_entries() {
    let dir = TempDir::new("tar-slip");
    let archive = dir.path().join("evil.tar.gz");
    write_tar_gz(
        &archive,
        &[
            ("llama-b12110/llama-server", b"binary"),
            ("llama-b12110/../../../escaped.txt", b"pwned"),
        ],
    );

    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let result = crate::install::extract_archive_for_test(&archive, &staging, ArchiveKind::TarGz);
    assert!(matches!(
        result,
        Err(crate::LlamaError::UnsafeArchiveEntry { .. })
    ));
    assert!(!dir.path().join("escaped.txt").exists());
}

#[test]
fn extraction_accepts_a_realistic_windows_layout() {
    let dir = TempDir::new("zip-layout");
    let archive = dir.path().join("good.zip");
    write_zip(
        &archive,
        &[
            ("llama-server.exe", b"binary"),
            ("llama.dll", b"library"),
            ("LICENSE", b"mit"),
        ],
    );

    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    crate::install::extract_archive_for_test(&archive, &staging, ArchiveKind::Zip)
        .expect("clean archive must extract");
    assert_eq!(
        std::fs::read(staging.join("llama-server.exe")).unwrap(),
        b"binary"
    );
    assert_eq!(
        std::fs::read(staging.join("llama.dll")).unwrap(),
        b"library"
    );
}

#[test]
fn extraction_accepts_a_realistic_linux_layout() {
    let dir = TempDir::new("tar-layout");
    let archive = dir.path().join("good.tar.gz");
    write_tar_gz(
        &archive,
        &[
            ("llama-b12110/llama-server", b"binary"),
            ("llama-b12110/libllama.so.0.9.0", b"library"),
        ],
    );

    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    crate::install::extract_archive_for_test(&archive, &staging, ArchiveKind::TarGz)
        .expect("clean archive must extract");
    assert!(staging.join("llama-b12110/llama-server").is_file());
    assert!(staging.join("llama-b12110/libllama.so.0.9.0").is_file());
}

/// CUDA の cudart は 1 段ディレクトリを挟み、`llama-server` と同じ場所へ置く必要がある。
/// その組み合わせを実際のアーカイブと同じ形で再現する。
#[test]
fn extraction_places_the_cudart_archive_next_to_the_server() {
    let dir = TempDir::new("cudart-layout");
    let build = dir.path().join("llama.tar.gz");
    write_tar_gz(&build, &[("llama-b12110/llama-server", b"binary")]);
    let cudart = dir.path().join("cudart.tar.gz");
    write_tar_gz(
        &cudart,
        &[
            (
                "cudart-llama-b12110-bin-ubuntu-cuda-13.4-x64/libcudart.so.13",
                b"rt",
            ),
            (
                "cudart-llama-b12110-bin-ubuntu-cuda-13.4-x64/libcublas.so.13",
                b"blas",
            ),
        ],
    );

    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    crate::install::extract_archive_for_test(&build, &staging, ArchiveKind::TarGz)
        .expect("the llama build must extract");
    crate::install::extract_archive_with_layout_for_test(
        &cudart,
        &staging,
        ArchiveKind::TarGz,
        Some("cudart-llama-b12110-bin-ubuntu-cuda-13.4-x64"),
        Some("llama-b12110"),
    )
    .expect("the cudart archive must extract into the same directory");

    assert!(staging.join("llama-b12110/llama-server").is_file());
    assert_eq!(
        std::fs::read(staging.join("llama-b12110/libcudart.so.13")).unwrap(),
        b"rt"
    );
    assert!(staging.join("llama-b12110/libcublas.so.13").is_file());
    // 1 段ディレクトリが残っていないこと。
    assert!(
        !staging
            .join("cudart-llama-b12110-bin-ubuntu-cuda-13.4-x64")
            .exists()
    );
}

#[test]
fn extraction_rejects_an_archive_that_does_not_match_the_expected_prefix() {
    // 想定と違うレイアウトを黙って捨てると、cudart が入らないまま「起動はするが GPU を
    // 使わない」状態になる。展開時点で失敗させる。
    let dir = TempDir::new("cudart-wrong-prefix");
    let archive = dir.path().join("cudart.tar.gz");
    write_tar_gz(&archive, &[("unexpected/libcudart.so.13", b"rt")]);

    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let result = crate::install::extract_archive_with_layout_for_test(
        &archive,
        &staging,
        ArchiveKind::TarGz,
        Some("cudart-llama-b12110-bin-ubuntu-cuda-13.4-x64"),
        Some("llama-b12110"),
    );
    assert!(matches!(
        result,
        Err(crate::LlamaError::InvalidArchive { .. })
    ));
}

/// b12110 の Linux 版は `libllama.so.0 -> libllama.so.0.9.0` のような相対リンクを含み、
/// 動的リンカはこれを辿って実体を解決する。リンクを捨てると起動できない。
#[cfg(unix)]
#[test]
fn extraction_keeps_in_tree_relative_symlinks() {
    let dir = TempDir::new("tar-symlink");
    let archive = dir.path().join("good.tar.gz");
    write_tar_gz_with_symlinks(
        &archive,
        &[("llama-b12110/libllama.so.0.9.0", b"library")],
        &[("llama-b12110/libllama.so.0", "libllama.so.0.9.0")],
    );

    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    crate::install::extract_archive_for_test(&archive, &staging, ArchiveKind::TarGz)
        .expect("in-tree symlink must extract");

    let link = staging.join("llama-b12110/libllama.so.0");
    let metadata = std::fs::symlink_metadata(&link).expect("link must exist");
    assert!(metadata.file_type().is_symlink());
    assert_eq!(std::fs::read(&link).expect("link must resolve"), b"library");
}

#[cfg(unix)]
#[test]
fn extraction_rejects_symlinks_pointing_outside_the_tree() {
    let dir = TempDir::new("tar-symlink-escape");
    let archive = dir.path().join("evil.tar.gz");
    write_tar_gz_with_symlinks(
        &archive,
        &[("llama-b12110/llama-server", b"binary")],
        &[("llama-b12110/escape", "../../../etc/passwd")],
    );

    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let result = crate::install::extract_archive_for_test(&archive, &staging, ArchiveKind::TarGz);
    assert!(matches!(
        result,
        Err(crate::LlamaError::UnsafeArchiveEntry { .. })
    ));
    assert!(!staging.join("llama-b12110/escape").exists());
}

#[cfg(unix)]
#[test]
fn extraction_rejects_absolute_symlink_targets() {
    let dir = TempDir::new("tar-symlink-absolute");
    let archive = dir.path().join("evil.tar.gz");
    write_tar_gz_with_symlinks(
        &archive,
        &[("llama-b12110/llama-server", b"binary")],
        &[("llama-b12110/escape", "/etc/passwd")],
    );

    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let result = crate::install::extract_archive_for_test(&archive, &staging, ArchiveKind::TarGz);
    assert!(matches!(
        result,
        Err(crate::LlamaError::UnsafeArchiveEntry { .. })
    ));
    assert!(!staging.join("llama-b12110/escape").exists());
}

/// リンク先の字句解決。プラットフォーム非依存なので Windows でも実行する。
#[test]
fn symlink_targets_are_resolved_lexically_within_the_extraction_root() {
    use std::path::Path;

    let resolve = crate::install::resolve_link_within_for_test;

    // b12110 の Linux 版に実際に含まれる形。同じディレクトリ内の実体を指す。
    assert_eq!(
        resolve(Path::new("llama-b12110"), "libllama.so.0.9.0"),
        Some(PathBuf::from("llama-b12110/libllama.so.0.9.0"))
    );
    // 1 段上へ戻るのは、ルート内に留まる限り許す。
    assert_eq!(
        resolve(Path::new("llama-b12110/sub"), "../libllama.so"),
        Some(PathBuf::from("llama-b12110/libllama.so"))
    );
    assert_eq!(
        resolve(Path::new("llama-b12110"), ".."),
        Some(PathBuf::new())
    );

    // ルートより上へ出るものは拒否する。
    assert_eq!(resolve(Path::new("llama-b12110"), "../../etc/passwd"), None);
    assert_eq!(resolve(Path::new(""), "../escape"), None);
    assert_eq!(resolve(Path::new("a/b"), "../../../escape"), None);
    // 絶対パスも拒否する。
    assert_eq!(resolve(Path::new("llama-b12110"), "/etc/passwd"), None);
    assert_eq!(resolve(Path::new("llama-b12110"), "\\etc\\passwd"), None);
}

#[test]
fn installed_runtime_reports_only_existing_binaries() {
    let dir = TempDir::new("installed");
    let build = &RUNTIME_BUILDS[0];
    assert!(installed_runtime(build, dir.path()).is_none());

    let server = dir
        .path()
        .join(build.install_dir_name())
        .join(build.server_relative_path());
    std::fs::create_dir_all(server.parent().unwrap()).unwrap();
    std::fs::write(&server, b"binary").unwrap();

    assert_eq!(installed_runtime(build, dir.path()), Some(server));
}

#[tokio::test]
async fn remove_runtime_is_idempotent() {
    let dir = TempDir::new("remove");
    let build = &RUNTIME_BUILDS[0];
    remove_runtime(build, dir.path())
        .await
        .expect("removing a missing runtime must succeed");

    let server = dir
        .path()
        .join(build.install_dir_name())
        .join(build.server_relative_path());
    std::fs::create_dir_all(server.parent().unwrap()).unwrap();
    std::fs::write(&server, b"binary").unwrap();
    assert!(installed_runtime(build, dir.path()).is_some());

    remove_runtime(build, dir.path()).await.expect("remove");
    assert!(installed_runtime(build, dir.path()).is_none());
}

#[tokio::test]
async fn ensure_runtime_returns_the_existing_binary_without_network() {
    let dir = TempDir::new("ensure-existing");
    // 実在しないアセットを持つ偽ビルド。ネットワークへ出たら必ず失敗する。
    let build = RuntimeBuild {
        tag: PINNED_TAG,
        target: "windows-x86_64",
        backend: Backend::Cpu,
        cuda: None,
        archives: &[RuntimeArchive {
            asset: "does-not-exist.zip",
            size: 1,
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
            archive: ArchiveKind::Zip,
            strip_prefix: None,
            destination: None,
        }],
        server_relative_path: "llama-server.exe",
    };
    let server = dir
        .path()
        .join(build.install_dir_name())
        .join(build.server_relative_path());
    std::fs::create_dir_all(server.parent().unwrap()).unwrap();
    std::fs::write(&server, b"binary").unwrap();

    let resolved = ensure_runtime(&build, dir.path(), |_, _| {}, CancellationToken::new())
        .await
        .expect("already installed runtime must resolve offline");
    assert_eq!(resolved, server);
}

// ---------------------------------------------------------------------------
// サーバー設定
// ---------------------------------------------------------------------------

fn config_for(backend: Backend, slots: u8, ctx_per_slot: u32) -> ServerConfig {
    let mut config = ServerConfig::new("llama-server.exe", "model.gguf", backend);
    config.slots = slots;
    config.ctx_per_slot = ctx_per_slot;
    config
}

#[test]
fn arguments_use_the_flags_verified_against_b12110() {
    let config = config_for(Backend::Cpu, 2, 8_192);
    let args = config.arguments(12345, "secret");
    let joined = args.join(" ");

    assert!(joined.contains("--host 127.0.0.1"));
    assert!(joined.contains("--port 12345"));
    assert!(joined.contains("--api-key secret"));
    assert!(joined.contains("-m model.gguf"));
    assert!(joined.contains("-np 2"));
    // 総コンテキストは slots * ctx_per_slot。
    assert!(joined.contains("-c 16384"));
    // Web UI は無効化する。
    assert!(joined.contains("--no-ui"));
    // スロットごとに独立した KV を持たせる。
    assert!(joined.contains("--no-kv-unified"));
    // CPU はデバイスを使わず、レイヤーもオフロードしない。
    assert!(joined.contains("--device none"));
    assert!(joined.contains("-ngl 0"));
    assert!(!joined.contains("-ngl all"));
}

#[test]
fn gpu_arguments_offload_every_layer() {
    for backend in [
        Backend::Vulkan,
        Backend::Cuda(CudaVariant::Cuda12),
        Backend::Cuda(CudaVariant::Cuda13),
    ] {
        let config = config_for(backend, 1, 4_096);
        let joined = config.arguments(12345, "secret").join(" ");
        assert!(joined.contains("-ngl all"), "{backend:?}");
        assert!(!joined.contains("--device none"), "{backend:?}");
        assert!(joined.contains("-c 4096"), "{backend:?}");
    }
}

#[test]
fn slot_and_context_bounds_are_enforced() {
    assert!(
        config_for(Backend::Cpu, 0, 8_192)
            .validate_for_test()
            .is_err()
    );
    assert!(
        config_for(Backend::Cpu, 5, 8_192)
            .validate_for_test()
            .is_err()
    );
    assert!(config_for(Backend::Cpu, 1, 0).validate_for_test().is_err());

    let config = ServerConfig::new("llama-server.exe", "model.gguf", Backend::Cpu);
    assert_eq!(config.slots, 1);
    assert_eq!(config.ctx_per_slot, crate::server::DEFAULT_CTX_PER_SLOT);
    assert_eq!(config.total_ctx(), 8_192);

    assert!(config.clone().with_slots(4).is_ok());
    assert!(config.clone().with_slots(5).is_err());
    assert!(config.clone().with_ctx_per_slot(0).is_err());
}

// ---------------------------------------------------------------------------
// マネージャの再利用・再起動判断
// ---------------------------------------------------------------------------

/// 起動回数を数える偽ランチャー。
struct FakeLauncher {
    spawned: AtomicUsize,
    live: Mutex<Vec<Arc<FakeHandle>>>,
    next_port: AtomicU64,
}

impl FakeLauncher {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            spawned: AtomicUsize::new(0),
            live: Mutex::new(Vec::new()),
            next_port: AtomicU64::new(9_000),
        })
    }

    fn spawned_count(&self) -> usize {
        self.spawned.load(Ordering::Acquire)
    }

    fn live_handles(&self) -> Vec<Arc<FakeHandle>> {
        self.live.lock().unwrap().clone()
    }
}

struct FakeHandle {
    base_url: String,
    api_key: String,
    running: std::sync::atomic::AtomicBool,
    shutdowns: AtomicUsize,
}

impl ServerHandle for Arc<FakeHandle> {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn api_key(&self) -> &str {
        &self.api_key
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    fn shutdown(&self) -> crate::server::BoxFuture<'_, ()> {
        Box::pin(async move {
            self.shutdowns.fetch_add(1, Ordering::AcqRel);
            self.running.store(false, Ordering::Release);
        })
    }

    fn shutdown_blocking(&self) {
        self.shutdowns.fetch_add(1, Ordering::AcqRel);
        self.running.store(false, Ordering::Release);
    }
}

impl ServerLauncher for FakeLauncher {
    fn spawn<'a>(
        &'a self,
        _config: &'a ServerConfig,
    ) -> crate::server::BoxFuture<'a, crate::Result<Box<dyn ServerHandle>>> {
        Box::pin(async move {
            self.spawned.fetch_add(1, Ordering::AcqRel);
            let port = self.next_port.fetch_add(1, Ordering::AcqRel);
            let handle = Arc::new(FakeHandle {
                base_url: format!("http://127.0.0.1:{port}"),
                api_key: format!("key-{port}"),
                running: std::sync::atomic::AtomicBool::new(true),
                shutdowns: AtomicUsize::new(0),
            });
            self.live.lock().unwrap().push(Arc::clone(&handle));
            Ok(Box::new(handle) as Box<dyn ServerHandle>)
        })
    }
}

#[tokio::test]
async fn manager_reuses_a_live_server_with_the_same_config() {
    let launcher = FakeLauncher::new();
    let manager = RuntimeManager::with_launcher(launcher.clone(), Duration::from_secs(600));
    let config = config_for(Backend::Cpu, 1, 8_192);

    let first = manager.ensure_server(&config).await.expect("first start");
    let first_url = first.base_url().to_string();
    drop(first);

    let second = manager.ensure_server(&config).await.expect("reuse");
    assert_eq!(second.base_url(), first_url);
    assert_eq!(launcher.spawned_count(), 1, "must not spawn twice");
    assert_eq!(
        launcher.live_handles()[0].shutdowns.load(Ordering::Acquire),
        0
    );
}

#[tokio::test]
async fn manager_restarts_when_the_config_changes() {
    let launcher = FakeLauncher::new();
    let manager = RuntimeManager::with_launcher(launcher.clone(), Duration::from_secs(600));

    let cpu = config_for(Backend::Cpu, 1, 8_192);
    let gpu = config_for(Backend::Vulkan, 1, 8_192);

    drop(manager.ensure_server(&cpu).await.expect("cpu start"));
    drop(manager.ensure_server(&gpu).await.expect("gpu start"));

    assert_eq!(launcher.spawned_count(), 2);
    let handles = launcher.live_handles();
    assert_eq!(handles[0].shutdowns.load(Ordering::Acquire), 1);
    assert_eq!(handles[1].shutdowns.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn manager_restarts_when_only_the_cuda_variant_changes() {
    // CUDA 12 と 13 は別のバイナリ・別のディレクトリなので、同じ GPU でも別サーバーになる。
    let launcher = FakeLauncher::new();
    let manager = RuntimeManager::with_launcher(launcher.clone(), Duration::from_secs(600));

    let cuda12 = config_for(Backend::Cuda(CudaVariant::Cuda12), 1, 8_192);
    let cuda13 = config_for(Backend::Cuda(CudaVariant::Cuda13), 1, 8_192);

    drop(manager.ensure_server(&cuda12).await.expect("cuda 12 start"));
    drop(manager.ensure_server(&cuda13).await.expect("cuda 13 start"));

    assert_eq!(launcher.spawned_count(), 2);
    assert_eq!(
        launcher.live_handles()[0].shutdowns.load(Ordering::Acquire),
        1
    );
}

#[tokio::test]
async fn manager_restarts_when_the_server_died() {
    let launcher = FakeLauncher::new();
    let manager = RuntimeManager::with_launcher(launcher.clone(), Duration::from_secs(600));
    let config = config_for(Backend::Cpu, 1, 8_192);

    drop(manager.ensure_server(&config).await.expect("first start"));
    launcher.live_handles()[0]
        .running
        .store(false, Ordering::Release);

    drop(manager.ensure_server(&config).await.expect("restart"));
    assert_eq!(launcher.spawned_count(), 2);
}

#[tokio::test]
async fn manager_serializes_concurrent_starts() {
    let launcher = FakeLauncher::new();
    let manager = RuntimeManager::with_launcher(launcher.clone(), Duration::from_secs(600));
    let config = config_for(Backend::Cpu, 2, 8_192);

    let mut tasks = Vec::new();
    for _ in 0..4 {
        let manager = manager.clone();
        let config = config.clone();
        tasks.push(tokio::spawn(async move {
            let lease = manager.ensure_server(&config).await.expect("lease");
            lease.base_url().to_string()
        }));
    }

    let mut urls = Vec::new();
    for task in tasks {
        urls.push(task.await.expect("join"));
    }

    assert_eq!(launcher.spawned_count(), 1, "only one server may start");
    assert!(urls.iter().all(|url| *url == urls[0]));
}

#[tokio::test]
async fn manager_shutdown_stops_the_server_and_is_idempotent() {
    let launcher = FakeLauncher::new();
    let manager = RuntimeManager::with_launcher(launcher.clone(), Duration::from_secs(600));
    let config = config_for(Backend::Cpu, 1, 8_192);

    drop(manager.ensure_server(&config).await.expect("start"));
    assert!(manager.is_running());

    manager.shutdown().await;
    assert!(!manager.is_running());
    manager.shutdown().await;
    assert_eq!(
        launcher.live_handles()[0].shutdowns.load(Ordering::Acquire),
        1
    );
}

#[tokio::test]
async fn manager_stops_an_idle_server_after_the_timeout() {
    let launcher = FakeLauncher::new();
    let manager = RuntimeManager::with_launcher(launcher.clone(), Duration::from_millis(200));
    let config = config_for(Backend::Cpu, 1, 8_192);

    drop(manager.ensure_server(&config).await.expect("start"));
    assert!(manager.is_running());

    // リースを保持していないので、アイドル時間の経過で停止する。
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(!manager.is_running(), "idle server should have stopped");
    assert_eq!(
        launcher.live_handles()[0].shutdowns.load(Ordering::Acquire),
        1
    );
}

#[tokio::test]
async fn manager_does_not_stop_a_server_while_a_lease_is_held() {
    let launcher = FakeLauncher::new();
    let manager = RuntimeManager::with_launcher(launcher.clone(), Duration::from_millis(200));
    let config = config_for(Backend::Cpu, 1, 8_192);

    let lease = manager.ensure_server(&config).await.expect("start");
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(
        manager.is_running(),
        "a leased server must not be stopped for idleness"
    );
    drop(lease);
}

#[tokio::test]
async fn manager_blocking_shutdown_stops_the_server() {
    let launcher = FakeLauncher::new();
    let manager = RuntimeManager::with_launcher(launcher.clone(), Duration::from_secs(600));
    let config = config_for(Backend::Cpu, 1, 8_192);

    drop(manager.ensure_server(&config).await.expect("start"));
    manager.shutdown_blocking();
    assert!(!manager.is_running());
    manager.shutdown_blocking();
    assert_eq!(
        launcher.live_handles()[0].shutdowns.load(Ordering::Acquire),
        1
    );
}
