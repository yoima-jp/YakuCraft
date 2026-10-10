use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use yc_archive::normalize_archive_path;
use yc_language::{language_export_filename, write_language_file};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{
    ExportError, ExportInput, ExportKind, ExportValidationRequest, ValidationIssueCode,
    ValidationSeverity, atomic_write_file, language_filename_is_safe, language_format_for_filename,
    namespace_has_exportable_entries, validate_export, validate_zip, writer_options_for_namespace,
};

/// Modified JARの標準名を元JARのbasenameから作る。
pub fn modified_jar_filename(original_jar_path: impl AsRef<Path>) -> Option<String> {
    let stem = original_jar_path.as_ref().file_stem()?.to_str()?;
    Some(format!("{stem}-ja.jar"))
}

/// Source JARの他entryを圧縮データのまま複製して、翻訳entryだけ置き換える。
pub fn export_modified_jar(
    input: &ExportInput<'_>,
    original_jar_path: impl AsRef<Path>,
    output_path: impl AsRef<Path>,
    overwrite: bool,
) -> Result<PathBuf, ExportError> {
    let original_jar_path = original_jar_path.as_ref();
    let output_path = output_path.as_ref();
    validate_modified_jar_input(input, original_jar_path)?;
    export_modified_jar_validated(input, original_jar_path, output_path, overwrite)
}

pub(crate) fn validate_modified_jar_input(
    input: &ExportInput<'_>,
    original_jar_path: &Path,
) -> Result<(), ExportError> {
    let validation = ExportValidationRequest {
        input,
        kind: ExportKind::ModifiedJar,
        minecraft_version: None,
        manual_pack_format: None,
        original_jar_path: Some(original_jar_path),
    };
    for issue in validate_export(&validation) {
        if issue.severity != ValidationSeverity::Error {
            continue;
        }
        if issue.code == ValidationIssueCode::SignedJar {
            return Err(ExportError::SignedJar(original_jar_path.to_path_buf()));
        }
        return Err(ExportError::ValidationFailed(issue.message));
    }
    Ok(())
}

pub(crate) fn export_modified_jar_validated(
    input: &ExportInput<'_>,
    original_jar_path: &Path,
    output_path: &Path,
    overwrite: bool,
) -> Result<PathBuf, ExportError> {
    if output_path.exists()
        && std::fs::canonicalize(original_jar_path)? == std::fs::canonicalize(output_path)?
    {
        return Err(ExportError::SourceAndOutputSame(
            original_jar_path.to_path_buf(),
        ));
    }

    let mut generated = HashMap::<String, Vec<u8>>::new();
    let mut namespaces: Vec<_> = input
        .namespaces
        .iter()
        .filter(|namespace| {
            namespace.included && namespace_has_exportable_entries(input, &namespace.namespace)
        })
        .collect();
    namespaces.sort_by(|left, right| left.namespace.cmp(&right.namespace));
    for namespace in namespaces {
        let file_name = language_export_filename(
            input.language_files,
            &namespace.namespace,
            input.source_locale,
        )
        .ok_or_else(|| ExportError::UnsupportedLanguageFileFormat {
            namespace: namespace.namespace.clone(),
        })?;
        if !language_filename_is_safe(&file_name) {
            return Err(ExportError::UnsafeArchivePath(file_name));
        }
        // Modified JAR内でnamespaceごとの元ファイル形式を保ち、混在するModも書き出す。
        let format = language_format_for_filename(&namespace.namespace, &file_name)?;
        let target_path = format!("assets/{}/lang/{file_name}", namespace.namespace);
        if normalize_archive_path(&target_path).as_deref() != Ok(target_path.as_str()) {
            return Err(ExportError::UnsafeArchivePath(target_path));
        }
        let bytes = write_language_file(
            &namespace.namespace,
            input.entries,
            format,
            writer_options_for_namespace(input, &namespace.namespace),
        )?;
        if generated.insert(target_path.clone(), bytes).is_some() {
            return Err(ExportError::DuplicateTargetEntry(target_path));
        }
    }

    let expected_entries: Vec<_> = generated.keys().cloned().collect();
    let mut expected_entries = expected_entries;
    expected_entries.sort();
    atomic_write_file(
        output_path,
        overwrite,
        |file| copy_jar_with_replacements(original_jar_path, file, &generated),
        |path| validate_zip(path, &expected_entries),
    )?;
    tracing::info!(
        source = %original_jar_path.display(),
        destination = %output_path.display(),
        translated_entries = generated.len(),
        "modified JAR exported"
    );
    Ok(output_path.to_path_buf())
}

fn copy_jar_with_replacements(
    source_path: &Path,
    output: &mut File,
    generated: &HashMap<String, Vec<u8>>,
) -> Result<(), ExportError> {
    let source_file = File::open(source_path).map_err(|source| ExportError::WriteOutput {
        path: source_path.to_path_buf(),
        source,
    })?;
    let mut source = ZipArchive::new(source_file)?;
    let mut source_path_counts = HashMap::<String, usize>::with_capacity(source.len());
    for index in 0..source.len() {
        if let Some(name) = source.name_for_index(index) {
            *source_path_counts.entry(name.to_owned()).or_default() += 1;
        }
    }
    for path in generated.keys() {
        if source_path_counts.get(path).copied().unwrap_or_default() > 1 {
            return Err(ExportError::DuplicateTargetEntry(path.clone()));
        }
    }

    let mut replaced = HashSet::with_capacity(generated.len());
    let mut writer = ZipWriter::new(output);
    let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for index in 0..source.len() {
        let name = source
            .name_for_index(index)
            .ok_or_else(|| ExportError::UnsafeArchivePath(format!("entry #{index}")))?
            .to_owned();
        if let Some(bytes) = generated.get(&name) {
            writer.start_file(&name, deflate)?;
            writer.write_all(bytes)?;
            replaced.insert(name);
        } else {
            let entry = source.by_index_raw(index)?;
            writer.raw_copy_file(entry)?;
        }
    }
    let mut appended: Vec<_> = generated
        .iter()
        .filter(|(path, _)| !replaced.contains(*path))
        .collect();
    appended.sort_by(|left, right| left.0.cmp(right.0));
    for (path, bytes) in appended {
        writer.start_file(path, deflate)?;
        writer.write_all(bytes)?;
    }
    writer.finish()?;
    Ok(())
}
