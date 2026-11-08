//! 実ネットワークでのレジューム検証。
//!
//! `cargo run -p yc-llama --example verify_resume -- <cache_dir>`
//!
//! 1. モデルのダウンロードを途中でキャンセルし、`.part` が残ることを確認する
//! 2. 同じ宛先へ再実行し、Range リクエストで続きから取得して SHA-256 が一致することを確認する

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio_util::sync::CancellationToken;
use yc_llama::download_verified;

const MODEL_URL: &str = "https://huggingface.co/pfnet/plamo-3-translate-0.8b-GGUF/resolve/e10c327b7a7ce84ffc427cb4b09360b781429fda/plamo-3-translate-0.8b-Q4_K_M.gguf";
const MODEL_SIZE: u64 = 573_126_848;
const MODEL_SHA256: &str = "70891ec70dc7ab4205745a38307867c2881b99239f20327927633abfe4bc9ec6";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cache_dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: verify_resume <cache_dir>"),
    );
    std::fs::create_dir_all(&cache_dir)?;
    let dest = cache_dir.join("resume-test.gguf");
    let part = cache_dir.join("resume-test.gguf.part");
    let _ = std::fs::remove_file(&dest);
    let _ = std::fs::remove_file(&part);

    // --- 1. 途中でキャンセルする -------------------------------------------
    println!("=== first attempt (cancelled after 40 MiB) ===");
    let cancel = CancellationToken::new();
    let cancel_handle = cancel.clone();
    let seen = Arc::new(AtomicU64::new(0));
    let seen_writer = Arc::clone(&seen);
    let canceller = tokio::spawn(async move {
        loop {
            if seen_writer.load(Ordering::Acquire) >= 40 * 1024 * 1024 {
                cancel_handle.cancel();
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    let seen_reader = Arc::clone(&seen);
    let result = download_verified(
        MODEL_URL,
        &dest,
        MODEL_SIZE,
        MODEL_SHA256,
        move |received, _| {
            seen_reader.store(received, Ordering::Release);
        },
        cancel,
    )
    .await;
    let _ = canceller.await;
    assert!(
        matches!(result, Err(yc_llama::LlamaError::Cancelled)),
        "expected Cancelled, got {result:?}"
    );
    let part_len = std::fs::metadata(&part)?.len();
    println!("  cancelled; .part kept with {part_len} bytes");
    assert!(part_len > 0, "the partial file must be kept for resume");
    assert!(!dest.exists(), "the final file must not exist yet");

    // --- 2. 続きから取得する -----------------------------------------------
    println!("\n=== second attempt (resume) ===");
    let started = std::time::Instant::now();
    let first_report = Arc::new(Mutex::new(None::<u64>));
    let first = Arc::clone(&first_report);
    download_verified(
        MODEL_URL,
        &dest,
        MODEL_SIZE,
        MODEL_SHA256,
        move |received, total| {
            let mut guard = first.lock().unwrap();
            if guard.is_none() {
                *guard = Some(received);
            }
            if received == total {
                println!("  {received}/{total} bytes");
            }
        },
        CancellationToken::new(),
    )
    .await?;
    let resumed_from = first_report.lock().unwrap().unwrap_or(0);
    println!(
        "  resumed from {resumed_from} bytes and finished in {:.1}s",
        started.elapsed().as_secs_f64()
    );
    assert!(
        resumed_from >= part_len,
        "the resumed download should start at the existing prefix ({part_len}), got {resumed_from}"
    );
    assert_eq!(std::fs::metadata(&dest)?.len(), MODEL_SIZE);
    assert!(!part.exists(), ".part must be gone after success");
    println!("  size and SHA-256 verified");

    // 後始末（大きなファイルを残さない）。
    std::fs::remove_file(&dest)?;
    println!("\nresume check completed");
    Ok(())
}
