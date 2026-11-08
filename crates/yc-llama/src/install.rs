//! 固定ビルドのインストール（ダウンロード・検証・展開・配置）。
//!
//! 展開はアーカイブ内のパスをそのまま使わず、必ず「展開先のルートからの相対パス」として
//! 正規化してから書き込む。zip / tar のエントリ名に `../` や絶対パスを混ぜたアーカイブで
//! 展開先の外へ書き込まれる事故（zip-slip）を防ぐため。

use std::path::{Component, Path, PathBuf};

use tokio_util::sync::CancellationToken;

use crate::{
    LlamaError, Result,
    builds::{ArchiveKind, RuntimeBuild},
    download::download_verified,
};

/// アーカイブの一時ファイル名。インストール先と同じボリュームに置き、rename を原子的にする。
const ARCHIVE_FILE_PREFIX: &str = ".download-";
/// 展開中の一時ディレクトリ名。完成後に本来の名前へ rename する。
const STAGING_DIR_PREFIX: &str = ".staging-";

fn unique_suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// インストール済みディレクトリのパス（存在確認はしない）。
fn install_dir(build: &RuntimeBuild, runtime_root: &Path) -> PathBuf {
    runtime_root.join(build.install_dir_name())
}

/// アーカイブの一時ファイルのパス。
///
/// ビルドごとに固定名にして、キャンセルや通信断のあと `.part` からレジュームできるようにする。
fn archive_path_for(build: &RuntimeBuild, runtime_root: &Path) -> PathBuf {
    runtime_root.join(format!("{ARCHIVE_FILE_PREFIX}{}", build.asset))
}

/// インストール済みなら `llama-server` のパスを返す。ネットワークアクセスはしない。
pub fn installed_runtime(build: &RuntimeBuild, runtime_root: &Path) -> Option<PathBuf> {
    let server = install_dir(build, runtime_root).join(build.server_relative_path());
    server.is_file().then_some(server)
}

/// ランタイムを用意し、`llama-server` のパスを返す。
///
/// すでに展開済みならダウンロードも展開もせずに返す。未インストールならアーカイブを
/// 検証付きで取得し、一時ディレクトリへ展開してから本来の名前へ rename する。
/// インストール先ディレクトリが半端な状態で存在することはない（rename が原子的なため）。
/// キャンセル時はアーカイブの `.part` を残すので、次回の呼び出しでレジュームできる。
pub async fn ensure_runtime<F>(
    build: &RuntimeBuild,
    runtime_root: &Path,
    progress: F,
    cancel: CancellationToken,
) -> Result<PathBuf>
where
    F: Fn(u64, u64),
{
    if let Some(server) = installed_runtime(build, runtime_root) {
        return Ok(server);
    }

    tokio::fs::create_dir_all(runtime_root).await?;

    let archive_path = archive_path_for(build, runtime_root);
    let staging_dir = runtime_root.join(format!("{STAGING_DIR_PREFIX}{}", unique_suffix()));

    let result = install_inner(
        build,
        runtime_root,
        &archive_path,
        &staging_dir,
        progress,
        cancel,
    )
    .await;

    // 展開途中のディレクトリは成功・失敗にかかわらず残さない。
    let _ = tokio::fs::remove_dir_all(&staging_dir).await;
    // アーカイブは成功時だけ消す。失敗時は `.part` を残してレジュームに使う。
    if result.is_ok() {
        let _ = tokio::fs::remove_file(&archive_path).await;
    }
    result
}

async fn install_inner<F>(
    build: &RuntimeBuild,
    runtime_root: &Path,
    archive_path: &Path,
    staging_dir: &Path,
    progress: F,
    cancel: CancellationToken,
) -> Result<PathBuf>
where
    F: Fn(u64, u64),
{
    download_verified(
        &build.download_url(),
        archive_path,
        build.size,
        build.sha256,
        progress,
        cancel.clone(),
    )
    .await?;

    if cancel.is_cancelled() {
        return Err(LlamaError::Cancelled);
    }

    tokio::fs::create_dir_all(staging_dir).await?;
    let archive = archive_path.to_path_buf();
    let staging = staging_dir.to_path_buf();
    let kind = build.archive;
    // zip / tar の展開は同期 I/O なので、非同期ランタイムのワーカースレッドを塞がないよう
    // ブロッキングプールへ逃がす。
    tokio::task::spawn_blocking(move || extract_archive(&archive, &staging, kind))
        .await
        .map_err(|error| LlamaError::Io {
            message: error.to_string(),
        })??;

    let server_in_staging = staging_dir.join(build.server_relative_path());
    if !server_in_staging.is_file() {
        return Err(LlamaError::MissingServerBinary {
            path: build.server_relative_path().to_string(),
        });
    }
    set_executable_permissions(staging_dir, build.server_relative_path())?;

    let target = install_dir(build, runtime_root);
    // 前回の中断で残った半端なディレクトリがあれば消す。rename は空きディレクトリにしか
    // 行えないため、ここで確実に取り除いておく。
    if tokio::fs::try_exists(&target).await.unwrap_or(false) {
        tokio::fs::remove_dir_all(&target).await?;
    }
    tokio::fs::rename(staging_dir, &target).await?;

    let server = target.join(build.server_relative_path());
    if !server.is_file() {
        return Err(LlamaError::MissingServerBinary {
            path: build.server_relative_path().to_string(),
        });
    }
    Ok(server)
}

/// インストール済みランタイムを削除する。未インストールなら何もしない。
pub async fn remove_runtime(build: &RuntimeBuild, runtime_root: &Path) -> Result<()> {
    let dir = install_dir(build, runtime_root);
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// アーカイブのエントリ名を「展開ルートからの相対パス」として検証する。
///
/// 絶対パス、`..`、ドライブプレフィックス、ルート要素を含むものは拒否する。
/// 空のパスや `.` だけのパス（ディレクトリ自身）は `None` を返して読み飛ばす。
fn safe_relative_path(raw: &str) -> Result<Option<PathBuf>> {
    // Windows の区切り文字で書かれたエントリも同じ規則で扱う。
    let normalized = raw.replace('\\', "/");
    // 先頭の `/` は絶対パス。`C:/...` も同様に弾く（下の Prefix 判定でも捕まる）。
    if normalized.starts_with('/') {
        return Err(LlamaError::UnsafeArchiveEntry {
            entry: raw.to_string(),
        });
    }
    let mut relative = PathBuf::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => continue,
            ".." => {
                return Err(LlamaError::UnsafeArchiveEntry {
                    entry: raw.to_string(),
                });
            }
            _ => {}
        }
        let candidate = Path::new(component);
        // `C:` のようなドライブプレフィックスや、UNC の先頭要素を弾く。
        if candidate
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(LlamaError::UnsafeArchiveEntry {
                entry: raw.to_string(),
            });
        }
        relative.push(component);
    }
    if relative.as_os_str().is_empty() {
        return Ok(None);
    }
    Ok(Some(relative))
}

/// 展開先の外を指していないことを、正規化後のパスでも確認する。
fn resolve_within(root: &Path, relative: &Path) -> Result<PathBuf> {
    let joined = root.join(relative);
    // `root` は呼び出し側が作った空のディレクトリなので、字句的な前方一致で十分。
    if !joined.starts_with(root) {
        return Err(LlamaError::UnsafeArchiveEntry {
            entry: relative.display().to_string(),
        });
    }
    Ok(joined)
}

fn extract_archive(archive_path: &Path, dest: &Path, kind: ArchiveKind) -> Result<()> {
    match kind {
        ArchiveKind::Zip => extract_zip(archive_path, dest),
        ArchiveKind::TarGz => extract_tar_gz(archive_path, dest),
    }
}

fn extract_zip(archive_path: &Path, dest: &Path) -> Result<()> {
    use std::io;

    let file = std::fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| LlamaError::InvalidArchive {
        kind: "zip",
        message: error.to_string(),
    })?;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| LlamaError::InvalidArchive {
                kind: "zip",
                message: error.to_string(),
            })?;
        let raw_name = entry.name().unwrap_or_default().to_string();
        let Some(relative) = safe_relative_path(&raw_name)? else {
            continue;
        };
        let target = resolve_within(dest, &relative)?;

        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        // シンボリックリンクは展開しない。指し先が展開先の外（例: `/etc/passwd`）でも
        // リンク自体は作れてしまい、後続の書き込みが外へ抜ける足場になる。
        if entry.is_symlink() {
            return Err(LlamaError::UnsafeArchiveEntry { entry: raw_name });
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut output = std::fs::File::create(&target)?;
        io::copy(&mut entry, &mut output)?;
    }
    Ok(())
}

fn extract_tar_gz(archive_path: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| LlamaError::InvalidArchive {
            kind: "tar.gz",
            message: error.to_string(),
        })?;

    for entry in entries {
        let mut entry = entry.map_err(|error| LlamaError::InvalidArchive {
            kind: "tar.gz",
            message: error.to_string(),
        })?;
        let entry_type = entry.header().entry_type();
        let raw_name = String::from_utf8_lossy(&entry.path_bytes()).to_string();
        let Some(relative) = safe_relative_path(&raw_name)? else {
            continue;
        };
        let target = resolve_within(dest, &relative)?;

        if entry_type.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if entry_type.is_symlink() {
            // b12110 の Linux 版は `libllama.so.0 -> libllama.so.0.9.0` のような相対リンクを
            // 含み、動的リンカはこれを辿って実体を解決する。捨てると起動できない。
            // ただしリンク先が展開先の外を指すものは作らない（外への書き込みの足場になる）。
            let link_target = entry
                .link_name()
                .map_err(|error| LlamaError::InvalidArchive {
                    kind: "tar.gz",
                    message: error.to_string(),
                })?
                .map(|path| path.to_path_buf());
            let Some(link_target) = link_target else {
                return Err(LlamaError::UnsafeArchiveEntry { entry: raw_name });
            };
            let link_text = link_target.to_string_lossy().replace('\\', "/");
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            create_relative_symlink(dest, &target, &link_text, &raw_name)?;
            continue;
        }
        if !entry_type.is_file() {
            // ハードリンク・デバイスファイル・FIFO は作らない。
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry
            .unpack(&target)
            .map_err(|error| LlamaError::InvalidArchive {
                kind: "tar.gz",
                message: error.to_string(),
            })?;
    }
    Ok(())
}

/// リンク文字列を「リンクの親ディレクトリ（展開ルートからの相対）」からの相対として
/// 字句的に解決し、展開ルート内に収まるかを判定する。
///
/// 収まる場合は解決後の相対パスを返し、ルートより上へ出る場合は `None`。
/// プラットフォーム非依存にして、Windows 上でも単体テストできるようにしている。
/// 実際に使うのは `cfg(unix)` のシンボリックリンク展開だけ。
#[cfg_attr(not(unix), allow(dead_code))]
fn resolve_link_within(parent_relative: &Path, link_text: &str) -> Option<PathBuf> {
    if link_text.starts_with('/') || link_text.starts_with('\\') {
        return None;
    }
    let mut resolved: Vec<std::ffi::OsString> = parent_relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_os_string()),
            _ => None,
        })
        .collect();
    for component in link_text.replace('\\', "/").split('/') {
        match component {
            "" | "." => {}
            ".." => {
                // ルートより上へ出た時点で拒否する。
                resolved.pop()?;
            }
            value => resolved.push(std::ffi::OsString::from(value)),
        }
    }
    let mut path = PathBuf::new();
    for part in resolved {
        path.push(part);
    }
    Some(path)
}

/// アーカイブ内の相対シンボリックリンクを、展開先の中でだけ解決される形で作る。
///
/// リンクを辿った先が展開先の外にあると、後続の書き込みが外へ抜けてしまうため、
/// [`resolve_link_within`] でルート内に収まることを確認してから作る。
#[cfg(unix)]
fn create_relative_symlink(
    root: &Path,
    link_path: &Path,
    link_text: &str,
    raw_name: &str,
) -> Result<()> {
    let parent = link_path.parent().unwrap_or(root);
    let parent_relative =
        parent
            .strip_prefix(root)
            .map_err(|_| LlamaError::UnsafeArchiveEntry {
                entry: raw_name.to_string(),
            })?;
    if resolve_link_within(parent_relative, link_text).is_none() {
        return Err(LlamaError::UnsafeArchiveEntry {
            entry: raw_name.to_string(),
        });
    }
    std::os::unix::fs::symlink(link_text, link_path)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_relative_symlink(
    _root: &Path,
    _link_path: &Path,
    _link_text: &str,
    _raw_name: &str,
) -> Result<()> {
    // Windows ではシンボリックリンクを作らない（zip には含まれない）。
    Ok(())
}

/// Linux では展開したバイナリと共有ライブラリに実行権限が要る。
///
/// tar.gz のモードは tar 側が復元するが、zip は Windows 用でモードを持たないため、
/// ここで明示的に付与する。対象はサーバーバイナリと、そのディレクトリ直下の共有ライブラリ。
#[cfg(unix)]
fn set_executable_permissions(staging_dir: &Path, server_relative_path: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let server_path = staging_dir.join(server_relative_path);
    let dir = server_path.parent().unwrap_or(staging_dir);
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // シンボリックリンクはリンク先の実体に権限が付いていればよい。
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let is_server = path == server_path;
        // `libllama.so.0.9.0` のようにバージョン付きの共有ライブラリは拡張子が `0` になるため、
        // ファイル名の前方一致で判定する。
        let is_library = name.starts_with("lib") && name.contains(".so");
        if !is_server && !is_library {
            continue;
        }
        let mut permissions = entry.metadata()?.permissions();
        permissions.set_mode(permissions.mode() | 0o755);
        std::fs::set_permissions(&path, permissions)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_permissions(_staging_dir: &Path, _server_relative_path: &str) -> Result<()> {
    Ok(())
}

/// テスト専用の入口。`extract_archive` は非公開なので、安全性の検証だけを外へ出す。
#[cfg(test)]
pub(crate) fn extract_archive_for_test(
    archive_path: &Path,
    dest: &Path,
    kind: ArchiveKind,
) -> Result<()> {
    extract_archive(archive_path, dest, kind)
}

/// テスト専用の入口。
#[cfg(test)]
pub(crate) fn resolve_link_within_for_test(
    parent_relative: &Path,
    link_text: &str,
) -> Option<PathBuf> {
    resolve_link_within(parent_relative, link_text)
}
