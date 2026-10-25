use std::{collections::BTreeMap, path::Path};

use yc_archive::Archive;
use yc_language::{
    EntryFlag, is_empty_translation, language_export_filename, resource_pack_language_filename,
};

use crate::{
    ExportInput, ManualPackFormat, language_filename_is_safe, language_format_for_filename,
    namespace_has_exportable_entries, namespace_has_language_entries, pack_format_for_version,
};
use yc_validation::{PlaceholderProfile, compare_placeholders_for_profile};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportKind {
    LanguageFile,
    ResourcePack,
    ModifiedJar,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PlaceholderWarning {
    pub namespace: String,
    pub key: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationIssueCode {
    MalformedTranslationFile,
    PlaceholderWarning,
    UntranslatedEntries,
    DuplicateKeys,
    UnsupportedTargetFormat,
    MissingMinecraftVersion,
    UnknownPackFormat,
    UnsafeTargetPath,
    SignedJar,
    NestedJarAlreadyHasTarget,
    SourceJarUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationIssue {
    pub severity: ValidationSeverity,
    pub code: ValidationIssueCode,
    pub message: String,
}

pub struct ExportValidationRequest<'a> {
    pub input: &'a ExportInput<'a>,
    pub kind: ExportKind,
    pub minecraft_version: Option<&'a str>,
    pub manual_pack_format: Option<ManualPackFormat>,
    pub original_jar_path: Option<&'a Path>,
}

pub fn validate_export(request: &ExportValidationRequest<'_>) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    let included: Vec<_> = request
        .input
        .namespaces
        .iter()
        .filter(|namespace| namespace.included)
        .collect();
    let included_names: std::collections::HashSet<_> = included
        .iter()
        .map(|namespace| namespace.namespace.as_str())
        .collect();

    for path in request.input.malformed_translation_files {
        issues.push(issue(
            // A broken locale is skipped during analysis; valid locales still provide export data,
            // and the malformed source entry is copied unchanged into modified JARs.
            ValidationSeverity::Warning,
            ValidationIssueCode::MalformedTranslationFile,
            format!("translation file could not be parsed: {path}"),
        ));
    }

    let mut placeholder_keys = std::collections::HashSet::new();
    for warning in request.input.placeholder_warnings {
        if included_names.contains(warning.namespace.as_str())
            && placeholder_keys.insert((warning.namespace.clone(), warning.key.clone()))
        {
            issues.push(issue(
                ValidationSeverity::Warning,
                ValidationIssueCode::PlaceholderWarning,
                format!(
                    "placeholder mismatch: {}:{}",
                    warning.namespace, warning.key
                ),
            ));
        }
    }
    for entry in request.input.entries.iter().filter(|entry| {
        included_names.contains(entry.namespace.as_str())
            && entry.flags.contains(&EntryFlag::PlaceholderError)
    }) {
        if placeholder_keys.insert((entry.namespace.clone(), entry.key.clone())) {
            issues.push(issue(
                ValidationSeverity::Warning,
                ValidationIssueCode::PlaceholderWarning,
                format!("placeholder mismatch: {}:{}", entry.namespace, entry.key),
            ));
        }
    }
    for document in request
        .input
        .patchouli_documents
        .iter()
        .filter(|document| included_names.contains(document.namespace.as_str()))
    {
        for translation in document
            .translations
            .iter()
            .filter(|translation| !is_empty_translation(&translation.target_text))
        {
            if !compare_placeholders_for_profile(
                &translation.source_text,
                &translation.target_text,
                PlaceholderProfile::Patchouli,
            )
            .is_valid
                && placeholder_keys.insert((document.namespace.clone(), translation.key.clone()))
            {
                issues.push(issue(
                    ValidationSeverity::Warning,
                    ValidationIssueCode::PlaceholderWarning,
                    format!(
                        "placeholder mismatch: {}:{}",
                        document.namespace, translation.key
                    ),
                ));
            }
        }
    }

    for document in request.input.patchouli_documents.iter().filter(|document| {
        included_names.contains(document.namespace.as_str())
            && document
                .translations
                .iter()
                .any(|translation| !is_empty_translation(&translation.target_text))
    }) {
        let target =
            yc_language::patchouli_target_document_path(&document.source_path, "en_us", "ja_jp");
        if target.as_deref() != Some(document.target_path.as_str())
            || yc_archive::normalize_archive_path(&document.target_path).as_deref()
                != Ok(document.target_path.as_str())
        {
            issues.push(issue(
                ValidationSeverity::Error,
                ValidationIssueCode::UnsafeTargetPath,
                format!("unsafe Patchouli target path: {}", document.target_path),
            ));
        }
    }

    let untranslated_count = request
        .input
        .entries
        .iter()
        .filter(|entry| {
            included_names.contains(entry.namespace.as_str())
                && is_empty_translation(&entry.target_text)
        })
        .count();
    if untranslated_count > 0 {
        issues.push(issue(
            ValidationSeverity::Warning,
            ValidationIssueCode::UntranslatedEntries,
            format!("{untranslated_count} translation entries are empty"),
        ));
    }

    let mut keys_by_namespace = BTreeMap::<&str, BTreeMap<&str, usize>>::new();
    for entry in request
        .input
        .entries
        .iter()
        .filter(|entry| included_names.contains(entry.namespace.as_str()))
    {
        *keys_by_namespace
            .entry(&entry.namespace)
            .or_default()
            .entry(&entry.key)
            .or_default() += 1;
    }
    for (namespace, keys) in keys_by_namespace {
        let duplicates: Vec<_> = keys
            .into_iter()
            .filter_map(|(key, count)| (count > 1).then_some(key))
            .collect();
        if !duplicates.is_empty() {
            issues.push(issue(
                ValidationSeverity::Error,
                ValidationIssueCode::DuplicateKeys,
                format!(
                    "duplicate translation keys in {namespace}: {}",
                    duplicates.join(", ")
                ),
            ));
        }
    }

    if request.kind != ExportKind::ResourcePack {
        for namespace in &included {
            if !namespace_has_language_entries(request.input, &namespace.namespace) {
                continue;
            }
            let filename = language_export_filename(
                request.input.language_files,
                &namespace.namespace,
                request.input.source_locale,
            );
            if filename.is_none() {
                issues.push(issue(
                    ValidationSeverity::Error,
                    ValidationIssueCode::UnsupportedTargetFormat,
                    format!(
                        "no source language filename is available for {}",
                        namespace.namespace
                    ),
                ));
            } else if let Some(filename) = filename.as_ref() {
                if !language_filename_is_safe(filename) {
                    issues.push(issue(
                        ValidationSeverity::Error,
                        ValidationIssueCode::UnsafeTargetPath,
                        format!("target language filename is unsafe: {filename}"),
                    ));
                } else if language_format_for_filename(&namespace.namespace, filename).is_err() {
                    issues.push(issue(
                        ValidationSeverity::Error,
                        ValidationIssueCode::UnsupportedTargetFormat,
                        format!("unsupported target language filename: {filename}"),
                    ));
                }
            }
        }
    }

    if request.kind == ExportKind::ResourcePack {
        match request.minecraft_version {
            None => issues.push(issue(
                ValidationSeverity::Error,
                ValidationIssueCode::MissingMinecraftVersion,
                "Minecraft version is required for a Resource Pack".to_owned(),
            )),
            Some(version) => {
                if resource_pack_language_filename(Some(version)).is_none() {
                    issues.push(issue(
                        ValidationSeverity::Error,
                        ValidationIssueCode::UnsupportedTargetFormat,
                        format!("unsupported Resource Pack language format for {version}"),
                    ));
                }
                if pack_format_for_version(version).is_none()
                    && request.manual_pack_format.is_none()
                {
                    issues.push(issue(
                        ValidationSeverity::Error,
                        ValidationIssueCode::UnknownPackFormat,
                        format!("pack format is unknown for Minecraft {version}"),
                    ));
                }
            }
        }
    }

    if request.kind == ExportKind::ModifiedJar {
        validate_modified_jar(request, &included_names, &mut issues);
    }
    issues
}

fn validate_modified_jar(
    request: &ExportValidationRequest<'_>,
    included_names: &std::collections::HashSet<&str>,
    issues: &mut Vec<ValidationIssue>,
) {
    let Some(path) = request.original_jar_path else {
        issues.push(issue(
            ValidationSeverity::Error,
            ValidationIssueCode::SourceJarUnavailable,
            "source JAR path was not provided".to_owned(),
        ));
        return;
    };
    let mut archive = match Archive::open(path) {
        Ok(archive) => archive,
        Err(error) => {
            issues.push(issue(
                ValidationSeverity::Error,
                ValidationIssueCode::SourceJarUnavailable,
                format!("could not inspect source JAR: {error}"),
            ));
            return;
        }
    };
    if archive.is_signed() {
        issues.push(issue(
            ValidationSeverity::Error,
            ValidationIssueCode::SignedJar,
            format!("signed JAR cannot be modified: {}", path.display()),
        ));
        return;
    }
    let additional_nested_paths: std::collections::BTreeSet<_> = request
        .input
        .language_files
        .iter()
        .filter(|file| !file.container_path.is_empty())
        .map(|file| file.container_path.clone())
        .collect();
    let additional_nested_paths: Vec<_> = additional_nested_paths.into_iter().collect();
    let nested_archives = match archive.discover_nested(&additional_nested_paths) {
        Ok(nested) => nested,
        Err(error) => {
            issues.push(issue(
                ValidationSeverity::Error,
                ValidationIssueCode::SourceJarUnavailable,
                format!("could not inspect nested JARs: {error}"),
            ));
            return;
        }
    };
    for nested in nested_archives {
        let Some(nested_archive) = nested.archive else {
            continue;
        };
        let nested_namespaces: std::collections::BTreeSet<_> = request
            .input
            .language_files
            .iter()
            .filter(|file| {
                file.container_path == nested.path
                    && included_names.contains(file.namespace.as_str())
                    && namespace_has_exportable_entries(request.input, &file.namespace)
            })
            .map(|file| file.namespace.as_str())
            .collect();
        for namespace in nested_namespaces {
            let Some(target_name) = language_export_filename(
                request.input.language_files,
                namespace,
                request.input.source_locale,
            ) else {
                continue;
            };
            let target_path = format!("assets/{namespace}/lang/{target_name}");
            if nested_archive
                .list_entries()
                .iter()
                .any(|entry| entry.normalized_path.as_deref() == Some(target_path.as_str()))
            {
                issues.push(issue(
                    ValidationSeverity::Warning,
                    ValidationIssueCode::NestedJarAlreadyHasTarget,
                    format!(
                        "nested JAR {} already contains {target_path}; Resource Pack export is safer",
                        nested.path
                    ),
                ));
            }
        }
    }
}

fn issue(
    severity: ValidationSeverity,
    code: ValidationIssueCode,
    message: String,
) -> ValidationIssue {
    ValidationIssue {
        severity,
        code,
        message,
    }
}
