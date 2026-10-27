use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

use yc_archive::normalize_archive_path;
use yc_language::{language_export_filename, write_language_file};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{
    ExportError, ExportInput, ExportKind, ExportValidationRequest, PatchouliDocumentArtifact,
    ValidationIssueCode, ValidationSeverity, atomic_write_file, create_patchouli_document_files,
    language_filename_is_safe, language_format_for_filename, namespace_has_exportable_entries,
    namespace_has_language_entries, validate_export, validate_zip, writer_options_for_namespace,
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
        if !namespace_has_language_entries(input, &namespace.namespace) {
            continue;
        }
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

    let mut nested_documents = HashMap::<String, Vec<PatchouliDocumentArtifact>>::new();
    for artifact in create_patchouli_document_files(input)? {
        if artifact.container_path.is_empty() {
            if generated
                .insert(artifact.path.clone(), artifact.bytes)
                .is_some()
            {
                return Err(ExportError::DuplicateTargetEntry(artifact.path));
            }
        } else {
            nested_documents
                .entry(artifact.container_path.clone())
                .or_default()
                .push(artifact);
        }
    }
    for (container_path, documents) in nested_documents {
        if yc_archive::normalize_archive_path(&container_path).as_deref()
            != Ok(container_path.as_str())
        {
            return Err(ExportError::UnsafeArchivePath(container_path));
        }
        let source_file = File::open(original_jar_path)?;
        let mut source = ZipArchive::new(source_file)?;
        let mut nested = source.by_name(&container_path)?;
        let mut nested_bytes = Vec::new();
        nested.read_to_end(&mut nested_bytes)?;
        let rewritten = rewrite_nested_documents(&nested_bytes, &documents)?;
        if generated
            .insert(container_path.clone(), rewritten)
            .is_some()
        {
            return Err(ExportError::DuplicateTargetEntry(container_path));
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

fn rewrite_nested_documents(
    source_bytes: &[u8],
    documents: &[PatchouliDocumentArtifact],
) -> Result<Vec<u8>, ExportError> {
    let replacements: HashMap<_, _> = documents
        .iter()
        .map(|document| (document.path.as_str(), document.bytes.as_slice()))
        .collect();
    let mut source = ZipArchive::new(Cursor::new(source_bytes))?;
    let mut output = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut replaced = HashSet::new();
    for index in 0..source.len() {
        let mut entry = source.by_index(index)?;
        let name = entry.name()?.into_owned();
        if let Some(bytes) = replacements.get(name.as_str()) {
            output.start_file(&name, options)?;
            output.write_all(bytes)?;
            replaced.insert(name);
            continue;
        }
        if entry.is_dir() {
            output.add_directory(&name, options)?;
        } else {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            output.start_file(&name, options)?;
            output.write_all(&bytes)?;
        }
    }
    for document in documents {
        if replaced.contains(&document.path) {
            continue;
        }
        output.start_file(&document.path, options)?;
        output.write_all(&document.bytes)?;
    }
    Ok(output.finish()?.into_inner())
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
        if let Some(Ok(name)) = source.name_for_index(index) {
            *source_path_counts.entry(name.into_owned()).or_default() += 1;
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
            .and_then(Result::ok)
            .ok_or_else(|| ExportError::UnsafeArchivePath(format!("entry #{index}")))?
            .into_owned();
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
