//! 検証付きの汎用ダウンローダ。
//!
//! ランタイム（llama.cpp のアーカイブ）とモデル（GGUF）の両方で使う。どちらも数 MB〜数 GB あり、
//! 途中で失敗・中断したときに最初から落とし直すのは現実的でないため、`.part` ファイルへの
//! 追記と HTTP Range によるレジュームに対応する。
//!
//! 取得元は HTTPS に限定する。平文 HTTP や、HTTPS から HTTP へのリダイレクトを許すと、
//! 途中の経路で実行ファイルを差し替えられる。ただし最終的な防御は SHA-256 の照合であり、
//! HTTPS 制限はその手前の緩和策として置いている。

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant},
};

use reqwest::{
    Client, StatusCode, Url,
    header::{CONTENT_LENGTH, RANGE},
};
use sha2::{Digest, Sha256};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt},
};
use tokio_util::sync::CancellationToken;

use crate::{LlamaError, Result};

/// 取得元へ送る User-Agent。GitHub と Hugging Face は UA 無しの要求を弾くことがある。
const USER_AGENT: &str = concat!("YakuCraft/", env!("CARGO_PKG_VERSION"));

/// 接続確立の待ち時間。回線が細い環境でも TCP/TLS の確立自体は速いはず。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// 1 回の読み取りの待ち時間。数 GB のファイルでも「無通信」がこれ以上続けば失敗とみなす。
/// 総ダウンロード時間の上限は設けない（低速回線で完走できなくなるため）。
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// 進捗通知の間隔。UI 更新の頻度を抑えつつ、固まって見えない程度に出す。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const PROGRESS_BYTES: u64 = 1024 * 1024;

const COPY_BUFFER: usize = 64 * 1024;

/// ダウンロード用の共有 HTTP クライアント。
///
/// 接続プールを再利用できるようプロセス内で 1 つだけ作る。生成に失敗した場合は
/// 呼び出しごとに作り直さず、そのままエラーを返す（設定の問題で毎回失敗するため）。
fn client() -> Result<&'static Client> {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let built = Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error("redirect limit exceeded");
            }
            // 署名付き CDN（GitHub の objects host、HF の cas-bridge）へは https のまま飛ぶ。
            // https 以外へ落とすリダイレクトは追わずに失敗させる。
            if attempt.url().scheme() == "https" {
                attempt.follow()
            } else {
                attempt.error("redirect to a non-https URL")
            }
        }))
        .build()
        .map_err(|_| LlamaError::Network)?;
    Ok(CLIENT.get_or_init(|| built))
}

/// ダウンロード URL がこのモジュールのポリシーを満たすかだけを確認する。
pub fn validate_download_url(url: &str) -> Result<()> {
    parse_download_url(url, false).map(|_| ())
}

/// `allow_http` はテスト専用の抜け道。ローカルのテスト HTTP サーバーへ繋ぐためにだけ true にする。
fn parse_download_url(url: &str, allow_http: bool) -> Result<Url> {
    let parsed = Url::parse(url).map_err(|_| LlamaError::InsecureUrl)?;
    let scheme_ok = parsed.scheme() == "https" || (allow_http && parsed.scheme() == "http");
    if !scheme_ok || parsed.host_str().is_none() {
        return Err(LlamaError::InsecureUrl);
    }
    Ok(parsed)
}

/// `<dest>.part` のパス。
fn part_path_for(dest_path: &Path) -> PathBuf {
    let mut raw = dest_path.as_os_str().to_owned();
    raw.push(".part");
    PathBuf::from(raw)
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

async fn remove_file_if_exists(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// 部分ファイルを書き込み用に開き、既存の内容をハッシュへ取り込む。
///
/// レジューム時は「ダウンロード済みの先頭部分」も最終的な SHA-256 の対象なので、
/// 追記を始める前にここで読み込んでハッシュへ流し込む。
async fn prepare_part(part_path: &Path, resume_from: u64) -> Result<(File, Sha256)> {
    let mut hasher = Sha256::new();
    if resume_from == 0 {
        let file = File::create(part_path).await?;
        return Ok((file, hasher));
    }

    let mut existing = File::open(part_path).await?;
    let mut buffer = vec![0_u8; COPY_BUFFER];
    let mut hashed = 0_u64;
    loop {
        let read = existing.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        hashed += read as u64;
    }
    drop(existing);
    if hashed != resume_from {
        // 読み込み中にファイルが変わった。レジュームを諦めて先頭からやり直す。
        let file = File::create(part_path).await?;
        return Ok((file, Sha256::new()));
    }

    let file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(part_path)
        .await?;
    Ok((file, hasher))
}

/// 完成済みのファイルが期待どおりかを調べる。
///
/// ファイルが無ければ `None`。あれば `Some(true)`（サイズもハッシュも一致）か
/// `Some(false)`（不一致）。
async fn existing_matches(
    dest_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<Option<bool>> {
    let metadata = match tokio::fs::metadata(dest_path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Ok(Some(false));
    }
    if metadata.len() != expected_size {
        return Ok(Some(false));
    }

    let mut file = File::open(dest_path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; COPY_BUFFER];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(hex_lower(&hasher.finalize()) == expected_sha256))
}

/// URL から取得し、サイズと SHA-256 を検証して `dest_path` へ置く。
///
/// 進捗は `progress(received, total)` で通知する。`cancel` がキャンセルされた場合は
/// `.part` を残したまま [`LlamaError::Cancelled`] を返すので、次回の呼び出しでレジュームできる。
pub async fn download_verified<F>(
    url: &str,
    dest_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
    progress: F,
    cancel: CancellationToken,
) -> Result<()>
where
    F: Fn(u64, u64),
{
    download_verified_with(
        url,
        dest_path,
        expected_size,
        expected_sha256,
        progress,
        cancel,
        false,
    )
    .await
}

/// 実装本体。`allow_http` はテストでローカルのテストサーバーへ繋ぐためだけに使う。
#[allow(clippy::too_many_arguments)]
async fn download_verified_with<F>(
    url: &str,
    dest_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
    progress: F,
    cancel: CancellationToken,
    allow_http: bool,
) -> Result<()>
where
    F: Fn(u64, u64),
{
    let url = parse_download_url(url, allow_http)?;
    if cancel.is_cancelled() {
        return Err(LlamaError::Cancelled);
    }
    if let Some(parent) = dest_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let expected_sha256 = expected_sha256.trim().to_ascii_lowercase();

    // すでに完成済みで内容も一致していれば、ネットワークへ出ない。
    // 展開だけ失敗して再試行したときに、数十 MB を落とし直さずに済む。
    if let Some(existing) = existing_matches(dest_path, expected_size, &expected_sha256).await? {
        if existing {
            progress(expected_size, expected_size);
            return Ok(());
        }
        // 内容が違う完成ファイルは、新しい `.part` の検証が通るまで残す。
        // ここで消してしまうと、キャンセルや通信断で「以前は使えたファイル」まで失われる。
        // 置き換えは検証成功後の rename で原子的に行う。
        tracing::debug!("an existing file does not match; it will be replaced on success");
    }

    let part_path = part_path_for(dest_path);
    let mut resume_from = match tokio::fs::metadata(&part_path).await {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.into()),
    };
    if resume_from > expected_size {
        // 期待サイズを超える部分ファイルは別物か破損。レジュームせず捨てる。
        remove_file_if_exists(&part_path).await?;
        resume_from = 0;
    }
    let mut attempt = 0_u32;
    loop {
        attempt += 1;
        let (mut file, mut hasher) = prepare_part(&part_path, resume_from).await?;
        let mut received = resume_from;

        let mut request = client()?.get(url.clone());
        if resume_from > 0 {
            request = request.header(RANGE, format!("bytes={resume_from}-"));
        }
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(LlamaError::Cancelled),
            response = request.send() => response.map_err(|_| LlamaError::Network)?,
        };
        let status = response.status();

        if resume_from > 0 && status == StatusCode::RANGE_NOT_SATISFIABLE {
            // サーバ側のファイルが部分ファイルより小さい。レジュームは不可能なのでやり直す。
            if attempt >= 2 {
                return Err(LlamaError::HttpStatus {
                    status: status.as_u16(),
                });
            }
            remove_file_if_exists(&part_path).await?;
            resume_from = 0;
            continue;
        }
        if !status.is_success() {
            return Err(LlamaError::HttpStatus {
                status: status.as_u16(),
            });
        }
        if resume_from > 0 && status != StatusCode::PARTIAL_CONTENT {
            // サーバが Range を無視して全体を返した。先頭から書き直す。
            tracing::debug!("server ignored the Range request; restarting the download");
            resume_from = 0;
            received = 0;
            let reopened = prepare_part(&part_path, 0).await?;
            file = reopened.0;
            hasher = reopened.1;
        }
        if resume_from == 0 {
            // 途中まで落ちていた場合に備え、Content-Length が期待サイズと食い違えば早期に失敗させる。
            if let Some(length) = response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                && length != expected_size
            {
                return Err(LlamaError::SizeMismatch {
                    expected: expected_size,
                    actual: length,
                });
            }
        }

        progress(received, expected_size);
        let mut last_emit = Instant::now();
        let mut last_reported = received;
        let mut response = response;
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => return Err(LlamaError::Cancelled),
                chunk = response.chunk() => chunk.map_err(|_| LlamaError::Network)?,
            };
            let Some(chunk) = next else { break };
            received += chunk.len() as u64;
            if received > expected_size {
                // 期待より多く流れてきた。書き込み続ける前に打ち切る。
                drop(file);
                remove_file_if_exists(&part_path).await?;
                return Err(LlamaError::SizeMismatch {
                    expected: expected_size,
                    actual: received,
                });
            }
            file.write_all(&chunk).await?;
            hasher.update(&chunk);

            if last_emit.elapsed() >= PROGRESS_INTERVAL
                || received - last_reported >= PROGRESS_BYTES
            {
                progress(received, expected_size);
                last_emit = Instant::now();
                last_reported = received;
            }
        }

        file.flush().await?;
        // rename 前にディスクへ書き出しておく。電源断で「サイズは正しいが中身が欠けた」
        // ファイルがインストール済み扱いになるのを避ける。
        file.sync_all().await?;
        drop(file);

        if received != expected_size {
            remove_file_if_exists(&part_path).await?;
            return Err(LlamaError::SizeMismatch {
                expected: expected_size,
                actual: received,
            });
        }

        let actual_sha256 = hex_lower(&hasher.finalize());
        if actual_sha256 != expected_sha256 {
            // 壊れたデータは再利用できないので部分ファイルごと捨てる。
            remove_file_if_exists(&part_path).await?;
            return Err(LlamaError::ChecksumMismatch);
        }

        // 検証済みの `.part` を完成ファイルへ置き換える。`rename` は既存ファイルを
        // 上書きする（Windows でも `MoveFileEx` 相当で原子的に置き換わる）ため、
        // 先に消してから rename する必要はない。消してしまうと、その隙間に別プロセスが
        // 中途半端な状態を読んだり、rename が失敗して「どちらも無い」状態になり得る。
        tokio::fs::rename(&part_path, dest_path).await?;
        progress(expected_size, expected_size);
        return Ok(());
    }
}

/// テスト専用の入口。ローカルのテスト HTTP サーバー（http）へ繋げるようにする。
#[cfg(test)]
pub(crate) async fn download_verified_for_test<F>(
    url: &str,
    dest_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
    progress: F,
    cancel: CancellationToken,
) -> Result<()>
where
    F: Fn(u64, u64),
{
    download_verified_with(
        url,
        dest_path,
        expected_size,
        expected_sha256,
        progress,
        cancel,
        true,
    )
    .await
}
