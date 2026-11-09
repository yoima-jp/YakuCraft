//! 実ランタイム・実モデルを使う PLaMo の検証。
//!
//! `cargo test -p yakucraft --lib plamo_real -- --ignored --nocapture`
//!
//! 通常のテストスイートからは外している（ネットワーク・実時間・数 GB のファイルを使う）。
//! 実行内容は `start_ai_job` と同じ経路をできるだけなぞる:
//! 1. 状態 → 設定済み判定
//! 2. サーバ起動（CPU と GPU）
//! 3. `PlamoProvider` で数項目を翻訳
//! 4. 停止してプロセスが残らないこと
//!
//! モデルとランタイムは `%LOCALAPPDATA%\YakuCraft-dev-cache` の開発用キャッシュから
//! 一時データディレクトリへコピーする。無い場合はダウンロードする。

use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use tokio_util::sync::CancellationToken;
use yc_ai::{
    GlossaryTerm, TranslationContext, TranslationEntry, TranslationOptions, TranslationProvider,
    translate_entries,
};
use yc_llama::{current_target_build, ensure_runtime, installed_runtime};
use yc_plamo::{DEFAULT_MODEL_ID, PlamoProvider, PlamoProviderOptions, default_model};
use yc_project::AppSettings;

use crate::plamo::{self, PlamoDevice, PlamoState};

/// 開発用キャッシュ。実モデルと b12110 のランタイムが入っている。
fn dev_cache_dir() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    let path = PathBuf::from(local).join("YakuCraft-dev-cache");
    path.is_dir().then_some(path)
}

/// 検証用のデータディレクトリ。`YAKUCRAFT_DATA_DIR` と同じレイアウトにする。
fn prepare_data_dir(cache: &Path) -> PathBuf {
    let data_dir = std::env::temp_dir().join(format!(
        "yakucraft-plamo-real-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let plamo_root = data_dir.join("plamo");
    let models_dir = plamo_root.join("models");
    let runtime_dir = plamo_root.join("runtime");
    fs::create_dir_all(&models_dir).expect("models directory");
    fs::create_dir_all(&runtime_dir).expect("runtime directory");

    // キャッシュ内のモデルを探す。開発キャッシュは `models/` 直下と `yc-llama-verify/models/` の
    // 両方に置かれていることがある。
    let model = default_model();
    let candidates = [
        cache.join("models").join(model.file),
        cache
            .join("yc-llama-verify")
            .join("models")
            .join(model.file),
    ];
    if let Some(source) = candidates.iter().find(|path| path.is_file()) {
        fs::copy(source, models_dir.join(model.file)).expect("copy the cached model");
        println!("  model copied from {}", source.display());
    } else {
        println!("  no cached model; the download step will fetch it");
    }

    // ランタイムは展開済みディレクトリをそのまま持ってくる。`ensure_runtime` は
    // `<runtime_root>/<install_dir_name>/<server_relative_path>` を見る。
    for device in PlamoDevice::ALL {
        let Some(build) = device.backend().and_then(current_target_build) else {
            continue;
        };
        if installed_runtime(build, &runtime_dir).is_some() {
            continue;
        }
        let candidates = [
            cache.join("yc-llama-verify").join("runtimes"),
            cache.join("runtimes"),
        ];
        let Some(source) = candidates
            .iter()
            .map(|root| root.join(build.install_dir_name()))
            .find(|path| path.is_dir())
        else {
            continue;
        };
        let destination = runtime_dir.join(build.install_dir_name());
        copy_dir(&source, &destination).expect("copy the cached runtime");
        println!("  runtime copied from {}", source.display());
    }

    data_dir
}

/// 訳文が日本語かどうか。かな・漢字・々〆のいずれかを含めば日本語とみなす。
fn is_japanese(character: char) -> bool {
    matches!(
        character as u32,
        0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0x3005 | 0x3006
    )
}

fn copy_dir(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn accepted_settings() -> AppSettings {
    let mut settings = AppSettings::new();
    settings
        .values
        .insert(plamo::license_key(), serde_json::Value::Bool(true));
    settings.values.insert(
        plamo::model_key(),
        serde_json::Value::String(DEFAULT_MODEL_ID.to_owned()),
    );
    settings
}

/// `start_ai_job` が組み立てるのと同じ設定でサーバを立て、数項目を翻訳する。
async fn translate_with_device(plamo: &PlamoState, device: PlamoDevice) {
    let mut settings = accepted_settings();
    settings.values.insert(
        plamo::device_key(),
        serde_json::Value::String(device.as_str().to_owned()),
    );
    settings
        .values
        .insert(plamo::concurrency_key(), serde_json::Value::from(2usize));

    // 1. 状態 → 設定済み
    assert!(
        plamo::configured(plamo, &settings),
        "the runtime and model must be installed for {device:?}"
    );
    let config = plamo::run_config(plamo, &settings).expect("run config");
    let server_config = plamo::server_config(&config, 2).expect("server config");
    println!(
        "  {device:?}: model={} slots={} ctx/slot={}",
        config.model_id, server_config.slots, server_config.ctx_per_slot
    );

    // 2. サーバ起動
    let started = Instant::now();
    let lease = plamo
        .runtime()
        .ensure_server(&server_config)
        .await
        .unwrap_or_else(|error| panic!("{device:?} server did not start: {error}"));
    println!(
        "  {device:?}: server ready at {} in {:.2}s",
        lease.base_url(),
        started.elapsed().as_secs_f64()
    );
    plamo.set_active_model(config.model_path.clone());

    // 3. 翻訳（`start_ai_job` と同じ Provider 構築）
    let provider = PlamoProvider::new(
        lease.base_url(),
        Some(lease.api_key().to_owned()),
        PlamoProviderOptions::default(),
    )
    .expect("provider");
    provider
        .test_connection(&CancellationToken::new())
        .await
        .expect("health check and one completion");

    let entries = vec![
        TranslationEntry::new(1, "item.apple", "Apple", "en_us", "ja_jp"),
        TranslationEntry::new(2, "item.iron_ingot", "Iron Ingot", "en_us", "ja_jp"),
        TranslationEntry::new(3, "block.stone", "Stone", "en_us", "ja_jp"),
    ];
    let context = TranslationContext {
        mod_name: "Test Mod".into(),
        namespace: "test".into(),
        custom_prompt: None,
        glossary_terms: vec![GlossaryTerm {
            source_term: "Apple".into(),
            target_term: "リンゴ".into(),
        }],
    };
    let outcome = translate_entries(
        &provider,
        &entries,
        &context,
        &CancellationToken::new(),
        TranslationOptions { concurrency: 1 },
        None,
    )
    .await
    .expect("translation");
    println!("  {device:?}: accepted {} entries", outcome.accepted.len());
    for translation in &outcome.accepted {
        println!("    {} -> {}", translation.id, translation.translation);
    }
    assert!(
        !outcome.accepted.is_empty(),
        "{device:?} produced no translations"
    );
    for translation in &outcome.accepted {
        // 訳文が日本語になったことだけを見る。短い単語は仮名を含まず漢字だけになる
        // （"Stone" -> "石"）ため、かなだけで判定しない。
        assert!(
            translation.translation.chars().any(is_japanese),
            "{device:?} expected Japanese output, got {:?}",
            translation.translation
        );
    }

    // 4. 停止。削除経路と同じく管理者ごと作り直し、次の起動でもアイドル停止が効くことを
    // 確かめられる状態にしておく。
    drop(lease);
    plamo.shutdown_and_reset().await;
    assert!(!plamo.runtime().is_running(), "the server must be stopped");
    plamo.clear_active_model();
    println!("  {device:?}: stopped");
}

#[tokio::test]
#[ignore = "uses the real llama-server runtime and the 0.8B model; run deliberately"]
async fn plamo_real_runtime_translates_on_cpu_and_gpu() {
    let cache = dev_cache_dir().unwrap_or_else(|| {
        println!("no dev cache at %LOCALAPPDATA%\\YakuCraft-dev-cache; downloading instead");
        PathBuf::new()
    });
    let data_dir = prepare_data_dir(&cache);
    println!("data dir: {}", data_dir.display());
    let plamo = PlamoState::new(&data_dir);

    // このマシンで使えるデバイスだけを対象にする。CUDA はドライバーと GPU 次第で、
    // 使えない環境では `backend()` が `None` になる。
    let devices: Vec<PlamoDevice> = PlamoDevice::ALL
        .into_iter()
        .filter(|device| device.backend().is_some())
        .collect();
    println!("devices: {devices:?}");

    // キャッシュが無い/足りない場合はダウンロードする（数 GB になることがある）。
    for device in &devices {
        let build = device
            .backend()
            .and_then(current_target_build)
            .expect("current platform build");
        if installed_runtime(build, &plamo.runtime_dir).is_none() {
            ensure_runtime(
                build,
                &plamo.runtime_dir,
                |received, total| println!("  runtime {received}/{total}"),
                CancellationToken::new(),
            )
            .await
            .expect("runtime download");
        }
    }
    let model = default_model();
    if !plamo::model_downloaded(&plamo.models_dir, model) {
        yc_llama::download_verified(
            &model.download_url(),
            &plamo::model_path(&plamo.models_dir, model),
            model.size_bytes,
            model.sha256,
            |received, total| println!("  model {received}/{total}"),
            CancellationToken::new(),
        )
        .await
        .expect("model download");
    }

    for device in devices {
        println!("\n=== {device:?} ===");
        translate_with_device(&plamo, device).await;
    }

    // 終了時と同じ経路で止めても、二重停止にならないこと。
    plamo.runtime().shutdown_blocking();
    assert!(!plamo.runtime().is_running());

    let _ = fs::remove_dir_all(&data_dir);
}
