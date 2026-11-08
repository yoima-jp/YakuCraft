//! 実バイナリ・実モデルを使う検証プログラム。
//!
//! `cargo run -p yc-llama --example verify_runtime -- <cache_dir> [--skip-download]`
//!
//! ネットワークと実時間を使うため、通常のテストスイートからは外している。実行内容:
//! 1. CPU / Vulkan の b12110 をダウンロード・検証・展開する
//! 2. PLaMo 3 Translate 0.8B の GGUF をダウンロード・検証する
//! 3. CPU と GPU でサーバーを起動し、`POST /completion` を 1 回投げる
//! 4. GPU 実行でレイヤーが実際にオフロードされたことをログで確認する
//! 5. 親プロセスを強制終了して子プロセスが道連れになることを確認する

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use tokio_util::sync::CancellationToken;
use yc_llama::{
    Backend, DEFAULT_CTX_PER_SLOT, LlamaServer, ServerConfig, current_target_build,
    download_verified, ensure_runtime, installed_runtime,
};

const MODEL_URL: &str = "https://huggingface.co/pfnet/plamo-3-translate-0.8b-GGUF/resolve/e10c327b7a7ce84ffc427cb4b09360b781429fda/plamo-3-translate-0.8b-Q4_K_M.gguf";
const MODEL_SIZE: u64 = 573_126_848;
const MODEL_SHA256: &str = "70891ec70dc7ab4205745a38307867c2881b99239f20327927633abfe4bc9ec6";

const PROMPT: &str = "<|plamo:tag|>dataset<|plamo:msg|>translation<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>Iron Ingot<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let cache_dir = PathBuf::from(
        args.next()
            .expect("usage: verify_runtime <cache_dir> [--skip-download]"),
    );
    let skip_download = args.any(|arg| arg == "--skip-download");
    std::fs::create_dir_all(&cache_dir)?;
    let runtime_root = cache_dir.join("runtimes");
    let models_dir = cache_dir.join("models");
    std::fs::create_dir_all(&models_dir)?;
    println!("cache dir: {}", cache_dir.display());

    // --- 1. ランタイムの取得 ------------------------------------------------
    let mut servers = Vec::new();
    for backend in [Backend::Cpu, Backend::Gpu] {
        let build =
            current_target_build(backend).unwrap_or_else(|| panic!("no build for {backend:?}"));
        println!("\n=== {} ({}) ===", build.asset, build.install_dir_name());
        let started = Instant::now();
        let server_path = if skip_download {
            installed_runtime(build, &runtime_root)
                .expect("--skip-download requires an installed runtime")
        } else {
            let last_report = std::sync::Arc::new(std::sync::Mutex::new(Instant::now()));
            let report = std::sync::Arc::clone(&last_report);
            ensure_runtime(
                build,
                &runtime_root,
                move |received, total| {
                    let mut last = report.lock().unwrap();
                    if last.elapsed() >= Duration::from_secs(2) {
                        println!("  {received}/{total} bytes");
                        *last = Instant::now();
                    }
                },
                CancellationToken::new(),
            )
            .await?
        };
        println!(
            "  installed in {:.1}s: {}",
            started.elapsed().as_secs_f64(),
            server_path.display()
        );
        servers.push((backend, server_path));
    }

    // --- 2. モデルの取得 ----------------------------------------------------
    let model_path = models_dir.join("plamo-3-translate-0.8b-Q4_K_M.gguf");
    if skip_download && model_path.is_file() {
        println!("\nmodel already present: {}", model_path.display());
    } else {
        println!("\n=== model ===");
        let started = Instant::now();
        download_verified(
            MODEL_URL,
            &model_path,
            MODEL_SIZE,
            MODEL_SHA256,
            |received, total| {
                if received == total || received % (64 * 1024 * 1024) < 1024 * 1024 {
                    println!("  {received}/{total} bytes");
                }
            },
            CancellationToken::new(),
        )
        .await?;
        println!(
            "  downloaded and verified in {:.1}s",
            started.elapsed().as_secs_f64()
        );
    }

    // --- 3. CPU / GPU で起動して 1 回推論する -------------------------------
    for (backend, server_path) in &servers {
        println!("\n=== starting llama-server ({backend:?}) ===");
        // 既定の verbosity 3 ではレイヤーのオフロード状況がログに出ない（4 以上で出る）。
        // 本番のフラグ構成は変えずに、子プロセスへ継承される環境変数でログを詳しくする。
        // SAFETY: この検証プログラムは単一スレッドで起動直後に設定するだけで、
        // 他のスレッドが環境変数を読んでいる最中ではない。
        unsafe { std::env::set_var("LLAMA_ARG_LOG_VERBOSITY", "4") };

        let config = ServerConfig::new(server_path.clone(), model_path.clone(), *backend)
            .with_slots(2)?
            .with_ctx_per_slot(DEFAULT_CTX_PER_SLOT)?;
        let started = Instant::now();
        let server = LlamaServer::start(&config).await?;
        println!(
            "  ready in {:.2}s at {}",
            started.elapsed().as_secs_f64(),
            server.base_url()
        );

        // ログ読み取りタスクは非同期なので、起動直後は行がまだ届いていないことがある。
        // オフロードの行はモデル読み込み中に出るため、少し待ってから読む。
        let mut log = server.startup_log();
        for _ in 0..20 {
            if log.contains("layers to GPU") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
            log = server.startup_log();
        }
        if *backend == Backend::Gpu {
            let evidence = log
                .lines()
                .find(|line| line.contains("offloaded") && line.contains("layers to GPU"))
                .unwrap_or("<no offload line>");
            println!("  offload evidence: {}", evidence.trim());
            assert!(
                evidence.contains("layers to GPU"),
                "GPU run did not offload any layer"
            );
            // コンソールウィンドウが出ていないことの確認。GUI アプリの子プロセスは
            // CREATE_NO_WINDOW が無いと可視ウィンドウを持つ。
            let windows = visible_windows_of("llama-server.exe");
            println!("  visible llama-server windows: {windows}");
            assert_eq!(windows, 0, "llama-server opened a visible console window");
        } else {
            let device = log
                .lines()
                .find(|line| line.contains("loaded CPU backend") || line.contains("using device"))
                .unwrap_or("<no device line>");
            println!("  device evidence: {}", device.trim());
        }

        let content = completion(server.base_url(), server.api_key()).await?;
        println!("  POST /completion -> {content:?}");
        assert!(
            content
                .chars()
                .any(|c| ('\u{3040}'..='\u{30ff}').contains(&c)),
            "expected Japanese output, got {content:?}"
        );

        server.shutdown().await;
        println!("  stopped");
    }

    // --- 4. 親が死んだら子も死ぬこと ----------------------------------------
    // `orphan_parent` の例は LlamaServer を起動して待機するだけのプロセス。
    // これを強制終了し、Job Object の KILL_ON_JOB_CLOSE で llama-server も消えるかを見る。
    // この検証は必須なので、ヘルパーが無ければビルドして用意する（黙って飛ばさない）。
    println!("\n=== orphan check (Job Object) ===");
    let (backend, server_path) = &servers[0];
    let parent_exe = ensure_orphan_parent_built()?;
    let mut parent = std::process::Command::new(&parent_exe)
        .arg(server_path)
        .arg(&model_path)
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let parent_pid = parent.id();
    println!("  parent={parent_pid} backend={backend:?}");
    // 起動を待ってから強制終了する。kill は TerminateProcess 相当で、
    // 子プロセスへ終了要求を送らない。残るかどうかが Job Object の効果そのもの。
    tokio::time::sleep(Duration::from_secs(10)).await;
    let children_before = child_pids(parent_pid);
    println!("  llama-server children before kill: {children_before:?}");
    assert!(
        !children_before.is_empty(),
        "the parent did not start a llama-server; nothing to verify"
    );
    let _ = parent.kill();
    let _ = parent.wait();
    tokio::time::sleep(Duration::from_secs(4)).await;
    let survivors: Vec<&String> = children_before
        .iter()
        .filter(|pid| process_alive(pid))
        .collect();
    println!("  survivors after parent kill: {survivors:?}");
    assert!(
        survivors.is_empty(),
        "llama-server outlived its parent: {survivors:?}"
    );
    println!("  Job Object killed the child with the parent");

    println!("\nall checks completed");
    Ok(())
}

/// `orphan_parent` の例をビルドし、実行ファイルのパスを返す。
///
/// この検証は必須なので、見つからなければ `cargo build` して用意する。ビルドに失敗した場合は
/// 検証を飛ばさずにエラーにする。
fn ensure_orphan_parent_built() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let exe_name = if cfg!(windows) {
        "orphan_parent.exe"
    } else {
        "orphan_parent"
    };
    let sibling = std::env::current_exe()?
        .parent()
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(exe_name);
    if sibling.is_file() {
        return Ok(sibling);
    }

    println!("  building the orphan_parent helper");
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "yc-llama", "--example", "orphan_parent"])
        .status()?;
    if !status.success() {
        return Err("failed to build the orphan_parent helper".into());
    }
    if !sibling.is_file() {
        return Err(format!(
            "the orphan_parent helper was not produced at {}",
            sibling.display()
        )
        .into());
    }
    Ok(sibling)
}

/// 指定した実行ファイル名のプロセスが持つ可視ウィンドウ数。
///
/// `CREATE_NO_WINDOW` が効いていれば 0 になる。効いていないとコンソールウィンドウが 1 つ出る。
fn visible_windows_of(process_name: &str) -> usize {
    let script = format!(
        "$p = Get-Process -Name '{stem}' -ErrorAction SilentlyContinue; \
         if ($p) {{ ($p | Where-Object {{ $_.MainWindowHandle -ne 0 }}).Count }} else {{ 0 }}",
        stem = process_name.trim_end_matches(".exe")
    );
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap_or(0),
        Err(_) => 0,
    }
}

/// 指定 PID を親に持つ `llama-server.exe` の PID 一覧。
///
/// 名前だけで探すと、無関係なアプリ（Mozc など）が起動している llama-server を拾ってしまう。
fn child_pids(parent_pid: u32) -> Vec<String> {
    let script = format!(
        "Get-CimInstance Win32_Process -Filter \"Name='llama-server.exe' AND ParentProcessId={parent_pid}\" \
         | Select-Object -ExpandProperty ProcessId"
    );
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn process_alive(pid: &str) -> bool {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output();
    match output {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout);
            !text.trim().is_empty() && !text.contains("No tasks")
        }
        Err(_) => false,
    }
}

async fn completion(base_url: &str, api_key: &str) -> Result<String, Box<dyn std::error::Error>> {
    let body = format!(
        r#"{{"prompt":{},"temperature":0,"n_predict":64,"stop":["<|plamo:tag|>","<|plamo:bos|>"]}}"#,
        serde_json::to_string(PROMPT)?
    );
    let response = reqwest::Client::new()
        .post(format!("{base_url}/completion"))
        .bearer_auth(api_key)
        .header("content-type", "application/json")
        .body(body)
        .timeout(Duration::from_secs(120))
        .send()
        .await?
        .error_for_status()?;
    let value: serde_json::Value = serde_json::from_str(&response.text().await?)?;
    Ok(value["content"].as_str().unwrap_or_default().to_string())
}
