//! GPU の検出。
//!
//! 「GPU を選んだのに CPU で動いていた」を避けるため、CUDA が使えるかどうかを起動前に
//! 判定できる材料をここへ集める。判定は 2 つだけ:
//!
//! - NVIDIA の GPU・ドライバー・compute capability（`nvidia-smi` の CSV 出力）
//! - Vulkan ローダーの有無（`vulkan-1.dll` / `libvulkan.so.1`）
//!
//! どちらも外部プロセス 1 回とファイル存在確認だけで済むため、追加の依存は要らない。
//! 結果はプロセス内で 1 度だけ取り、アプリの寿命いずれも同じ値を使う（ドライバーの
//! 入れ替えはアプリの再起動を伴う）。
//!
//! ドライバーは `nvidia-smi --query-gpu=driver_version` が返す**完全な版**で比べる
//! （Windows は `591.74`、Linux は `570.26` や `550.54.14`）。メジャー番号だけで比べると、
//! Windows の `551.23` が CUDA 12.4 の Toolkit 下限 `551.61` を満たすと誤判定し、PTX の
//! JIT に失敗する構成を「提供する」ことになる。下限そのものは OS ごとに違うため、
//! [`crate::builds::CudaToolchain`] が OS 別の値を持ち、比較は
//! [`crate::builds::DriverVersion`] の成分ごとの数値で行う。
//!
//! 判定は検出結果（[`GpuInfo`]）だけを見る関数に分けてある（[`cuda_backend_for`] /
//! [`vulkan_backend_for`]）。単体テストが実機の `nvidia-smi` や Vulkan ローダーへ
//! 依存しないようにするため。実機を見るのは [`detect_gpu_info`] と [`gpu_info`] だけ。

use std::{
    path::PathBuf,
    process::{Command, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};

use crate::builds::{
    Backend, CudaToolchain, CudaVariant, DriverVersion, TargetOs, current_target_build,
};

/// `nvidia-smi` の待ち時間。ドライバーが応答しない環境で UI を止めないための上限。
const NVIDIA_SMI_TIMEOUT: Duration = Duration::from_secs(3);

/// 検出した NVIDIA GPU 1 台分。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NvidiaGpu {
    /// 表示名（例: `NVIDIA GeForce RTX 3060`）。
    pub name: String,
    /// ドライバーの完全な版（例: `591.74` なら `591.74`、`550.54.14` ならそのまま）。
    ///
    /// 下限との比較はこの値で行う（[`crate::builds::DriverVersion`]）。メジャー番号だけを
    /// 持つと、`551.23` が `551.61` を満たすと誤判定してしまう。
    pub driver: DriverVersion,
    /// compute capability（例: `8.6` なら `(8, 6)`）。
    ///
    /// `nvidia-smi` の `compute_cap` 列は比較的新しいドライバーにしか無い。古いドライバーでは
    /// `None` になり、その場合は CUDA 13 を選ばない（7.5 以上を確認できないため）。
    pub compute_capability: Option<(u32, u32)>,
}

/// このマシンで使える GPU バックエンドの材料。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GpuInfo {
    /// 検出した NVIDIA GPU。`nvidia-smi` が返した順。
    ///
    /// 1 台だけを持つのは誤り。llama.cpp は `--device` を指定しない限り見えている GPU を
    /// すべて使うため、遅い 1 台が混ざっていれば系統の選択にも影響する（[`cuda_variant_for`]）。
    pub nvidia: Vec<NvidiaGpu>,
    /// Vulkan ローダーが存在するか。
    pub vulkan: bool,
}

impl GpuInfo {
    /// 表示用の代表 GPU（先頭）。UI に名前を出すためだけに使う。
    pub fn primary_gpu(&self) -> Option<&NvidiaGpu> {
        self.nvidia.first()
    }
}

/// プロセス内で 1 度だけ検出した結果。
static GPU_INFO: OnceLock<GpuInfo> = OnceLock::new();

/// このセッションの GPU 情報。最初の呼び出しで検出し、以降は同じ値を返す。
pub fn gpu_info() -> &'static GpuInfo {
    GPU_INFO.get_or_init(detect_gpu_info)
}

/// CUDA バックエンドが使えるなら、その系統を返す。
///
/// 見えているすべての GPU とドライバーから系統を選び、その系統のビルドがこのプラットフォームに
/// 存在するときだけ返す。CUDA が使えない環境では `None`。
pub fn cuda_backend() -> Option<Backend> {
    cuda_backend_for(gpu_info())
}

/// 検出結果を渡して CUDA バックエンドを選ぶ。[`cuda_backend`] の中身。
///
/// 検出を注入できるように分けてある。単体テストが実機の `nvidia-smi` を呼ばずに、
/// 系統の選択とビルドの有無の関係を確かめられる。
pub fn cuda_backend_for(info: &GpuInfo) -> Option<Backend> {
    let variant = cuda_variant_for(&info.nvidia)?;
    current_target_build(Backend::Cuda(variant))?;
    Some(Backend::Cuda(variant))
}

/// このマシンで Vulkan を実際に使えるなら返す。ローダーが無い、またはビルドが無ければ `None`。
///
/// ダウンロード可否とは別物。ビルド自体はプラットフォームに存在するので、ローダーが無い
/// 環境でも取得はできる（起動前のデバイス確認で失敗を伝える）。
pub fn vulkan_backend() -> Option<Backend> {
    vulkan_backend_for(gpu_info())
}

/// 検出結果を渡して Vulkan バックエンドを選ぶ。[`vulkan_backend`] の中身。
pub fn vulkan_backend_for(info: &GpuInfo) -> Option<Backend> {
    if !info.vulkan {
        return None;
    }
    current_target_build(Backend::Vulkan)?;
    Some(Backend::Vulkan)
}

/// 見えているすべての GPU とドライバーから CUDA の系統を選ぶ。満たさなければ `None`。
///
/// 13.x は新しいドライバーと Turing 以降の GPU を要求する代わりに、新しい GPU 向けの実コードを
/// 持つ。12.x は古いドライバーでも動くが、sm_120（Blackwell）向けの実コードを持たず PTX の
/// JIT になる。詳しい根拠は [`crate::builds`] のモジュールコメントを参照。
///
/// 判定は「すべての GPU」に対して行う。llama.cpp は `--device` を指定しない限り見えている
/// GPU をすべて使うため、1 台でも条件を満たさない GPU があればその系統は選べない。
/// 例えば 1 台目が RTX 4090（8.9）で 2 台目が GTX 1050（6.1）の構成では、13.x を選ぶと
/// 6.1 の GPU に実コードが無く PTX の JIT になる（動くが遅く、VRAM も余分に食う）。
///
/// ドライバーの下限は GPU ごとに違う。実コードを持つ GPU はマイナーバージョン互換の下限で
/// 動くが、PTX に頼る GPU は「PTX を JIT できるドライバー」が要る。NVIDIA の CUDA
/// Compatibility Guide は「PTX をコンパイルしたアプリは古いドライバーでは動かない」と
/// 明記しており、実コードが無い GPU でマイナーバージョン互換の下限（Linux 525.60.13 /
/// Windows 527.41）まで許すと、起動はするがカーネルが動かない構成を「提供する」と誤判定する。
///
/// 下限はプラットフォームごとの実際のアーカイブ（[`CudaToolchain`]）から取る。12 系は
/// Windows が 12.4、Linux が 12.8 で Toolkit のドライバー下限が違うため、系統だけで
/// 決めてはいけない。比較は完全な版で行い、成分ごとの数値で見る。
pub fn cuda_variant_for(gpus: &[NvidiaGpu]) -> Option<CudaVariant> {
    if gpus.is_empty() {
        return None;
    }
    // 新しい系統から順に試す。13.x の方が速いので優先する。
    [CudaVariant::Cuda13, CudaVariant::Cuda12]
        .into_iter()
        .find(|variant| build_supports_all(*variant, gpus))
}

/// この系統のビルドが、このプラットフォームで、すべての GPU を載せられるか。
fn build_supports_all(variant: CudaVariant, gpus: &[NvidiaGpu]) -> bool {
    // このプラットフォームにビルドが無ければ、そもそも提供しない。
    let Some(toolchain) = current_target_build(Backend::Cuda(variant)).and_then(|b| b.cuda) else {
        return false;
    };
    toolchain_supports_all(&toolchain, gpus, TargetOs::current())
}

/// 1 つのツールチェーンが、対象 OS で、すべての GPU を載せられるか。
///
/// 対象 OS を引数にしているのは、下限が OS ごとに違うため。単体テストが Windows と Linux の
/// 両方の下限を、実行中のプラットフォームに依存せず確かめられるようにしてある。
pub(crate) fn toolchain_supports_all(
    toolchain: &CudaToolchain,
    gpus: &[NvidiaGpu],
    os: TargetOs,
) -> bool {
    gpus.iter().all(|gpu| {
        let required = match gpu.compute_capability {
            Some(capability) => toolchain.minimum_driver_for_os(capability, os),
            // compute capability が取れない古いドライバーでは、実コードを持つかどうかを
            // 判定できない。PTX に頼るものとして扱い、PTX を JIT できるドライバーを要求する
            // （安全側）。PTX を積まないビルドでは動かせないので `false`。
            None => match toolchain.ptx_floor {
                Some(floor) => toolchain.minimum_driver_for_os(floor, os),
                None => return false,
            },
        };
        // 完全な版で比べる。メジャー番号だけだと 551.23 が 551.61 を満たしてしまう。
        gpu.driver >= required
    })
}

/// 実際に検出する。テストから直接呼べるよう公開している。
pub fn detect_gpu_info() -> GpuInfo {
    GpuInfo {
        nvidia: detect_nvidia_gpus(),
        vulkan: vulkan_loader_present(),
    }
}

fn detect_nvidia_gpus() -> Vec<NvidiaGpu> {
    // `compute_cap` 列は比較的新しいドライバーにしか無い。古い `nvidia-smi` は未知の列を
    // 受け付けず何も返さないため、その場合は列を減らして問い合わせ直す。ここで諦めると、
    // CUDA 12 で動くはずの機体が「NVIDIA GPU 無し」と判定されてしまう。
    if let Some(output) = query_nvidia_smi("name,driver_version,compute_cap") {
        let gpus = parse_nvidia_smi_output(&output);
        if !gpus.is_empty() {
            return gpus;
        }
    }
    query_nvidia_smi("name,driver_version")
        .map(|output| parse_nvidia_smi_output(&output))
        .unwrap_or_default()
}

/// `nvidia-smi --query-gpu=<fields> --format=csv,noheader` を実行して標準出力を返す。
fn query_nvidia_smi(fields: &str) -> Option<String> {
    let mut command = Command::new("nvidia-smi");
    command
        .arg(format!("--query-gpu={fields}"))
        .arg("--format=csv,noheader")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // GUI アプリなので、子プロセスのコンソールウィンドウが一瞬でも出てはいけない。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    run_capturing_stdout(command, NVIDIA_SMI_TIMEOUT)
}

/// `nvidia-smi` の CSV 出力から GPU をすべて取り出す。
///
/// 出力は `NVIDIA GeForce RTX 3060, 591.74, 8.6` の形。`compute_cap` 列を持たない古い
/// ドライバーでは列が 2 つになり、値が `[N/A]` になることもある。どちらも GPU 自体は
/// 検出できているので、compute capability を `None` として受け入れる。
///
/// ドライバーの版が読めない行は捨てる。版を「古いドライバー」と読み替えると、その GPU を
/// 無視して別の系統を選んでしまう（実機に無い GPU を勝手に足すのと同じ誤り）。
pub fn parse_nvidia_smi_output(output: &str) -> Vec<NvidiaGpu> {
    let mut gpus = Vec::new();
    for line in output.lines() {
        let mut fields = line.split(',').map(str::trim);
        let Some(name) = fields.next().filter(|name| !name.is_empty()) else {
            continue;
        };
        let Some(driver) = fields.next().and_then(DriverVersion::parse) else {
            continue;
        };
        let compute_capability = fields.next().and_then(parse_compute_capability);
        gpus.push(NvidiaGpu {
            name: name.to_owned(),
            driver,
            compute_capability,
        });
    }
    gpus
}

/// `8.6` のような compute capability を `(8, 6)` にする。`[N/A]` などは `None`。
fn parse_compute_capability(raw: &str) -> Option<(u32, u32)> {
    let (major, minor) = raw.trim().split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// 子プロセスを起動して標準出力を読む。`timeout` を過ぎたら強制終了して `None`。
fn run_capturing_stdout(mut command: Command, timeout: Duration) -> Option<String> {
    use std::io::Read;

    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            // 応答しないドライバーを待ち続けない。
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut output = String::new();
    child.stdout.take()?.read_to_string(&mut output).ok()?;
    Some(output)
}

/// Vulkan ローダーのライブラリが存在するか。
///
/// ドライバーが入っていてもローダーが無ければ Vulkan ビルドはデバイスを見つけられず、
/// CPU へ黙って落ちる。ここでは「ローダーがある」ことだけを安く確かめる。
fn vulkan_loader_present() -> bool {
    vulkan_loader_candidates().iter().any(|path| path.is_file())
}

fn vulkan_loader_candidates() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let root = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        vec![
            root.join("System32").join("vulkan-1.dll"),
            root.join("SysWOW64").join("vulkan-1.dll"),
        ]
    }
    #[cfg(not(windows))]
    {
        vec![
            PathBuf::from("/usr/lib/x86_64-linux-gnu/libvulkan.so.1"),
            PathBuf::from("/usr/lib/libvulkan.so.1"),
            PathBuf::from("/usr/lib64/libvulkan.so.1"),
        ]
    }
}
