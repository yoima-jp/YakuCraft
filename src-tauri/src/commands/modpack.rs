use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
};

use yc_modpack::{ModpackData, ModpackFileKind, ModpackScan, analyze_files};

use crate::error::AppError;

pub(crate) fn collect_instance_files(
    instance_root: &Path,
    scan: &ModpackScan,
) -> Result<BTreeMap<String, Vec<u8>>, AppError> {
    scan.files
        .iter()
        .map(|file| {
            let relative = safe_relative_path(&file.path)?;
            let path = instance_root.join(&relative);
            let bytes = fs::read(&path).map_err(|error| {
                AppError::new(
                    "modpack_file_read",
                    &format!(
                        "Modpack の翻訳ファイルを読み込めませんでした。ファイルを確認して再試行してください: {}",
                        path.display()
                    ),
                    Some(format!("{}: {error}", path.display())),
                )
            })?;
            Ok((file.path.clone(), bytes))
        })
        .collect()
}

pub(crate) fn write_modpack_source_archive(
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>, AppError> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (path, bytes) in files {
        safe_relative_path(path)?;
        archive
            .start_file(path, options)
            .map_err(modpack_archive_error)?;
        archive.write_all(bytes).map_err(modpack_archive_error)?;
    }
    archive
        .finish()
        .map(|archive| archive.into_inner())
        .map_err(modpack_archive_error)
}

pub(crate) fn read_modpack_archive(path: &Path) -> Result<BTreeMap<String, Vec<u8>>, AppError> {
    let file = fs::File::open(path).map_err(modpack_archive_error)?;
    let mut archive = zip::ZipArchive::new(file).map_err(modpack_archive_error)?;
    let mut files = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(modpack_archive_error)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().map_err(modpack_archive_error)?.into_owned();
        safe_relative_path(&name)?;
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(modpack_archive_error)?;
        files.insert(name, bytes);
    }
    Ok(files)
}

pub(crate) fn analyze_modpack_archive(
    path: &Path,
) -> Result<(yc_analysis::AnalysisData, ModpackData), AppError> {
    let files = read_modpack_archive(path)?;
    let modpack = analyze_files(&files).map_err(|error| {
        AppError::new(
            "modpack_analysis",
            "Modpack の翻訳ファイルを解析できませんでした。",
            Some(error.to_string()),
        )
    })?;
    let mut jar = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut has_language = false;
    for file in &modpack.scan.files {
        if file.kind != ModpackFileKind::KubeJsLanguage {
            continue;
        }
        let Some(bytes) = files.get(&file.path) else {
            continue;
        };
        has_language = true;
        jar.start_file(
            format!(
                "assets/{}/lang/{}.json",
                file.namespace.as_deref().unwrap_or_default(),
                file.locale
            ),
            options,
        )
        .map_err(modpack_archive_error)?;
        jar.write_all(bytes).map_err(modpack_archive_error)?;
    }
    if !has_language {
        jar.start_file("assets/yc_internal/lang/en_us.json", options)
            .map_err(modpack_archive_error)?;
        jar.write_all(br#"{"internal.placeholder":""}"#)
            .map_err(modpack_archive_error)?;
    }
    let bytes = jar.finish().map_err(modpack_archive_error)?.into_inner();
    let temp_path =
        std::env::temp_dir().join(format!("yakucraft-modpack-{}.jar", uuid::Uuid::new_v4()));
    let result = (|| {
        fs::write(&temp_path, bytes).map_err(modpack_archive_error)?;
        let mut analysis = yc_analysis::analyze_jar(
            &temp_path,
            &yc_analysis::CancellationToken::default(),
            |_| {},
        )
        .map_err(|error| {
            AppError::new(
                "modpack_analysis",
                "KubeJS の言語ファイルを解析できませんでした。",
                Some(error.to_string()),
            )
        })?;
        if !has_language {
            analysis
                .result
                .namespaces
                .retain(|namespace| namespace.namespace != "yc_internal");
            analysis
                .result
                .language_files
                .retain(|language| language.namespace != "yc_internal");
        }
        Ok((analysis, modpack))
    })();
    if let Err(error) = fs::remove_file(&temp_path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(path = %temp_path.display(), %error, "temporary modpack analysis archive could not be removed");
    }
    result
}

fn safe_relative_path(value: &str) -> Result<PathBuf, AppError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || value.contains('\0')
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AppError::new(
            "invalid_modpack_path",
            "Modpack のファイルパスが不正です。",
            Some(value.to_owned()),
        ));
    }
    Ok(path.to_path_buf())
}

fn modpack_archive_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        "modpack_archive_read",
        "保存した Modpack ファイルを読み込めませんでした。",
        Some(error.to_string()),
    )
}
