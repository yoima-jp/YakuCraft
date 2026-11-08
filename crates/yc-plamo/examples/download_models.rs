//! 評価用に 2B / 8B の GGUF を dev キャッシュへ落とす補助ツール。
//!
//! 本体の評価ハーネス（`eval_mods`）はモデルがキャッシュに無いと始まらない。ダウンロードは
//! 数 GB あり数分かかるので、評価の試行錯誤とは切り離して単体で流せるようにしている。
//!
//! ```text
//! cargo run -p yc-plamo --example download_models -- 2b 8b
//! ```

use std::path::PathBuf;

use tokio_util::sync::CancellationToken;
use yc_llama::download_verified;
use yc_plamo::{catalog, find_model};

/// 既存の dev キャッシュ。前のタスクがランタイムと 0.8B を置いている。
fn cache_root() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("YakuCraft-dev-cache")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let models_dir = cache_root().join("models");
    tokio::fs::create_dir_all(&models_dir).await?;

    let requested: Vec<String> = std::env::args().skip(1).collect();
    let selected: Vec<&'static catalog::PlamoModel> = if requested.is_empty() {
        catalog::models().iter().collect()
    } else {
        requested
            .iter()
            .map(|id| {
                find_model(id)
                    .or_else(|| {
                        catalog::models()
                            .iter()
                            .find(|model| model.parameters.eq_ignore_ascii_case(id))
                    })
                    .unwrap_or_else(|| panic!("unknown model id: {id}"))
            })
            .collect()
    };

    for model in selected {
        let dest = models_dir.join(model.file);
        if dest.is_file() {
            println!("{}: already present at {}", model.id, dest.display());
            continue;
        }
        println!(
            "{}: downloading {} bytes from {}",
            model.id,
            model.size_bytes,
            model.download_url()
        );
        let started = std::time::Instant::now();
        let last_report = std::cell::Cell::new(0_u64);
        download_verified(
            &model.download_url(),
            &dest,
            model.size_bytes,
            model.sha256,
            |received, total| {
                // 1 GiB 刻みでだけ進捗を出す。数 GB のログで埋もれさせない。
                let step = received / (1024 * 1024 * 1024);
                if step != last_report.get() {
                    last_report.set(step);
                    println!("  {received}/{total} bytes");
                }
            },
            CancellationToken::new(),
        )
        .await?;
        println!(
            "{}: done in {:.1}s -> {}",
            model.id,
            started.elapsed().as_secs_f64(),
            dest.display()
        );
    }
    Ok(())
}
