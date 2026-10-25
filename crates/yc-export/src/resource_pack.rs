use std::{
    cmp::Ordering,
    collections::HashMap,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use yc_language::{
    LanguageFileFormat, TranslationEntry, WriterOptions, compare_minecraft_versions,
    is_empty_translation, resource_pack_language_filename, write_language_file,
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{
    ExportError, ExportInput, ExportKind, ExportValidationRequest, ValidationSeverity,
    atomic_write_file, create_patchouli_document_files, namespace_has_exportable_entries,
    namespace_has_language_entries, validate_export, validate_zip, writer_options_for_namespace,
};

const RESOURCE_PACK_FORMATS: &str = include_str!("resource_pack_formats.json");

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ManualPackFormat {
    pub major: u32,
    pub minor: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackMetadataFormat {
    PackFormatOnly,
    FormatRange,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPackFormat {
    pub major: u32,
    pub minor: u32,
    pub metadata: PackMetadataFormat,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResourcePackMod {
    pub name: Option<String>,
    pub version: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct ResourcePackOptions<'a> {
    pub mods: &'a [ResourcePackMod],
    pub source_jar_filename: &'a str,
    pub minecraft_version: Option<&'a str>,
    pub manual_format: Option<ManualPackFormat>,
}

/// A mod and its translation data for one combined resource pack.
#[derive(Clone, Debug)]
pub struct MergedPackMod<'a> {
    pub input: ExportInput<'a>,
    pub info: ResourcePackMod,
    pub label: &'a str,
}

#[derive(Clone, Copy, Debug)]
pub struct MergedResourcePackOptions<'a> {
    pub minecraft_version: Option<&'a str>,
    pub manual_format: Option<ManualPackFormat>,
    pub pack_description: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedResourcePackConflict {
    pub namespace: String,
    pub key: String,
    pub kept_from: String,
    pub dropped_from: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergedResourcePackReport {
    pub output_path: PathBuf,
    pub conflicts: Vec<MergedResourcePackConflict>,
}

#[derive(Deserialize)]
struct FormatDatabase {
    formats: Vec<FormatRange>,
}

#[derive(Deserialize)]
struct FormatRange {
    from: String,
    to: String,
    pack_format: FormatValue,
    metadata: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FormatValue {
    Major(u32),
    MajorMinor([u32; 2]),
}

impl FormatValue {
    fn components(self) -> (u32, u32) {
        match self {
            Self::Major(major) => (major, 0),
            Self::MajorMinor([major, minor]) => (major, minor),
        }
    }
}

pub fn pack_format_for_version(version: &str) -> Option<ResolvedPackFormat> {
    let database: FormatDatabase = serde_json::from_str(RESOURCE_PACK_FORMATS).ok()?;
    database.formats.into_iter().find_map(|entry| {
        let after_start = compare_minecraft_versions(version, &entry.from)? != Ordering::Less;
        let before_end = compare_minecraft_versions(version, &entry.to)? != Ordering::Greater;
        let (major, minor) = entry.pack_format.components();
        (after_start && before_end).then_some(ResolvedPackFormat {
            major,
            minor,
            metadata: match entry.metadata.as_str() {
                "format_range" => PackMetadataFormat::FormatRange,
                _ => PackMetadataFormat::PackFormatOnly,
            },
        })
    })
}

pub fn resource_pack_filename(mods: &[ResourcePackMod], source_jar_filename: &str) -> String {
    let mod_name = filename_component(resource_pack_mod_name(mods, source_jar_filename));
    let version = mods
        .first()
        .and_then(|mod_info| mod_info.version.as_deref())
        .filter(|version| !version.trim().is_empty())
        .map(filename_component);

    match version {
        Some(version) => format!("{mod_name}-{version}-JP_EX.zip"),
        None => format!("{mod_name}-JP_EX.zip"),
    }
}

pub fn export_resource_pack(
    input: &ExportInput<'_>,
    options: ResourcePackOptions<'_>,
    output_directory: impl AsRef<Path>,
    overwrite: bool,
) -> Result<PathBuf, ExportError> {
    let validation = ExportValidationRequest {
        input,
        kind: ExportKind::ResourcePack,
        minecraft_version: options.minecraft_version,
        manual_pack_format: options.manual_format,
        original_jar_path: None,
    };
    if let Some(error) = validate_export(&validation)
        .into_iter()
        .find(|issue| issue.severity == ValidationSeverity::Error)
    {
        return match error.code {
            crate::ValidationIssueCode::MissingMinecraftVersion => {
                Err(ExportError::MissingMinecraftVersion)
            }
            crate::ValidationIssueCode::UnknownPackFormat => Err(ExportError::UnknownPackFormat(
                options.minecraft_version.unwrap_or_default().to_owned(),
            )),
            _ => Err(ExportError::ValidationFailed(error.message)),
        };
    }

    let version = options
        .minecraft_version
        .ok_or(ExportError::MissingMinecraftVersion)?;
    let pack_format = resolve_pack_format(version, options.manual_format)?;
    let file_name = resource_pack_language_filename(Some(version))
        .ok_or_else(|| ExportError::UnknownPackFormat(version.to_owned()))?;
    // 統合packは対象Minecraft versionが一つなので、出力形式とlocale表記もそのversionで決める。
    let language_format = if file_name.ends_with(".json") {
        LanguageFileFormat::Json
    } else {
        LanguageFileFormat::LegacyLang
    };

    let mut namespaces: Vec<_> = input
        .namespaces
        .iter()
        .filter(|namespace| {
            namespace.included && namespace_has_exportable_entries(input, &namespace.namespace)
        })
        .collect();
    namespaces.sort_by(|left, right| left.namespace.cmp(&right.namespace));

    let mut language_entries = Vec::with_capacity(namespaces.len());
    for namespace in namespaces {
        if !namespace_has_language_entries(input, &namespace.namespace) {
            continue;
        }
        let path = format!("assets/{}/lang/{file_name}", namespace.namespace);
        if yc_archive::normalize_archive_path(&path).as_deref() != Ok(path.as_str()) {
            return Err(ExportError::UnsafeArchivePath(path));
        }
        let bytes = write_language_file(
            &namespace.namespace,
            input.entries,
            language_format.clone(),
            writer_options_for_namespace(input, &namespace.namespace),
        )?;
        language_entries.push((path, bytes));
    }
    language_entries.extend(
        create_patchouli_document_files(input)?
            .into_iter()
            .map(|artifact| (artifact.path, artifact.bytes)),
    );

    let metadata = pack_metadata(
        pack_format,
        &format!(
            "{} Japanese Expansion Pack",
            resource_pack_mod_name(options.mods, options.source_jar_filename).trim()
        ),
    )?;
    let entry_count = language_entries.len() + 1;
    let output_path = output_directory.as_ref().join(resource_pack_filename(
        options.mods,
        options.source_jar_filename,
    ));

    write_resource_pack_archive(&output_path, &metadata, &language_entries, overwrite)?;
    tracing::info!(path = %output_path.display(), entries = entry_count, "resource pack exported");
    Ok(output_path)
}

/// Resolve the human-readable name used for combined pack files and metadata.
pub fn merged_resource_pack_display_name(project_name: &str) -> &str {
    let project_name = project_name.trim();
    if project_name.is_empty() || is_generated_mod_project_name(project_name) {
        "YakuCraft"
    } else {
        project_name
    }
}

/// Choose the suggested filename for a combined resource pack.
pub fn merged_resource_pack_filename(mods: &[MergedPackMod<'_>], project_name: &str) -> String {
    if let [single] = mods {
        return resource_pack_filename(std::slice::from_ref(&single.info), single.label);
    }
    format!(
        "{}-JP_EX.zip",
        filename_component(merged_resource_pack_display_name(project_name))
    )
}

fn is_generated_mod_project_name(name: &str) -> bool {
    name.strip_suffix(" 個の mod").is_some_and(|count| {
        !count.is_empty() && count.chars().all(|character| character.is_ascii_digit())
    })
}

/// Export translations from multiple mods as one resource pack.
pub fn export_merged_resource_pack(
    mods: &[MergedPackMod<'_>],
    options: MergedResourcePackOptions<'_>,
    output_path: &Path,
    overwrite: bool,
) -> Result<MergedResourcePackReport, ExportError> {
    for mod_data in mods {
        let validation = ExportValidationRequest {
            input: &mod_data.input,
            kind: ExportKind::ResourcePack,
            minecraft_version: options.minecraft_version,
            manual_pack_format: options.manual_format,
            original_jar_path: None,
        };
        if let Some(error) = validate_export(&validation)
            .into_iter()
            .find(|issue| issue.severity == ValidationSeverity::Error)
        {
            return Err(ExportError::ModExportFailed {
                mod_name: mod_data.label.to_owned(),
                message: error.message,
            });
        }
    }

    let version = options
        .minecraft_version
        .ok_or(ExportError::MissingMinecraftVersion)?;
    let pack_format = resolve_pack_format(version, options.manual_format)?;
    let file_name = resource_pack_language_filename(Some(version))
        .ok_or_else(|| ExportError::UnknownPackFormat(version.to_owned()))?;
    let language_format = if file_name.ends_with(".json") {
        LanguageFileFormat::Json
    } else {
        LanguageFileFormat::LegacyLang
    };

    // 統合packは対象Minecraft versionが一つなので、原文ファイルの拡張子が混在しても影響しない。
    let mut namespace_parse_escapes = HashMap::<String, bool>::new();
    for mod_data in mods {
        for namespace in mod_data.input.namespaces.iter().filter(|namespace| {
            namespace.included
                && namespace_has_exportable_entries(&mod_data.input, &namespace.namespace)
        }) {
            if !namespace_has_language_entries(&mod_data.input, &namespace.namespace) {
                namespace_parse_escapes
                    .entry(namespace.namespace.clone())
                    .or_insert(false);
                continue;
            }
            let source_parse_escapes =
                writer_options_for_namespace(&mod_data.input, &namespace.namespace)
                    .legacy_parse_escapes;
            namespace_parse_escapes
                .entry(namespace.namespace.clone())
                .and_modify(|enabled| *enabled |= source_parse_escapes)
                .or_insert(source_parse_escapes);
        }
    }

    let mut merged_entries = Vec::<TranslationEntry>::new();
    let mut entry_indices = HashMap::<(String, String), usize>::new();
    let mut entry_labels = HashMap::<(String, String), String>::new();
    let mut conflicts = Vec::new();
    for mod_data in mods {
        let included_namespaces: std::collections::HashSet<_> = mod_data
            .input
            .namespaces
            .iter()
            .filter(|namespace| namespace.included)
            .map(|namespace| namespace.namespace.as_str())
            .collect();
        for entry in mod_data.input.entries.iter().filter(|entry| {
            included_namespaces.contains(entry.namespace.as_str())
                && !is_empty_translation(&entry.target_text)
        }) {
            let key = (entry.namespace.clone(), entry.key.clone());
            if let Some(index) = entry_indices.get(&key).copied() {
                if merged_entries[index].target_text != entry.target_text {
                    conflicts.push(MergedResourcePackConflict {
                        namespace: entry.namespace.clone(),
                        key: entry.key.clone(),
                        kept_from: entry_labels[&key].clone(),
                        dropped_from: mod_data.label.to_owned(),
                    });
                }
            } else {
                entry_indices.insert(key.clone(), merged_entries.len());
                entry_labels.insert(key, mod_data.label.to_owned());
                merged_entries.push(entry.clone());
            }
        }
    }

    let mut namespaces: Vec<_> = namespace_parse_escapes.keys().collect();
    namespaces.sort();
    let mut language_entries = Vec::with_capacity(namespaces.len());
    for namespace in namespaces {
        if !merged_entries
            .iter()
            .any(|entry| entry.namespace == **namespace)
        {
            continue;
        }
        let path = format!("assets/{namespace}/lang/{file_name}");
        if yc_archive::normalize_archive_path(&path).as_deref() != Ok(path.as_str()) {
            return Err(ExportError::UnsafeArchivePath(path));
        }
        let entries: Vec<_> = merged_entries
            .iter()
            .filter(|entry| entry.namespace == *namespace)
            .cloned()
            .collect();
        let bytes = write_language_file(
            namespace,
            &entries,
            language_format.clone(),
            WriterOptions {
                legacy_parse_escapes: namespace_parse_escapes[namespace],
            },
        )?;
        language_entries.push((path, bytes));
    }

    let mut merged_documents = HashMap::<String, Vec<u8>>::new();
    for mod_data in mods {
        for artifact in create_patchouli_document_files(&mod_data.input)? {
            merged_documents
                .entry(artifact.path)
                .or_insert(artifact.bytes);
        }
    }
    let mut merged_documents: Vec<_> = merged_documents.into_iter().collect();
    merged_documents.sort_by(|left, right| left.0.cmp(&right.0));
    language_entries.extend(merged_documents);

    let default_description = format!("{} 個の mod の日本語訳", mods.len());
    let metadata = pack_metadata(
        pack_format,
        options.pack_description.unwrap_or(&default_description),
    )?;
    write_resource_pack_archive(output_path, &metadata, &language_entries, overwrite)?;
    tracing::info!(
        path = %output_path.display(),
        mods = mods.len(),
        entries = language_entries.len() + 1,
        conflicts = conflicts.len(),
        "merged resource pack exported"
    );
    Ok(MergedResourcePackReport {
        output_path: output_path.to_path_buf(),
        conflicts,
    })
}

pub(crate) fn resolve_pack_format(
    version: &str,
    manual: Option<ManualPackFormat>,
) -> Result<ResolvedPackFormat, ExportError> {
    if let Some(known) = pack_format_for_version(version) {
        return Ok(known);
    }
    let manual = manual.ok_or_else(|| ExportError::UnknownPackFormat(version.to_owned()))?;
    // 1.21.9でpack.mcmetaの必須項目が変わったため、未登録の将来versionもこの境界で書式を選ぶ。
    let metadata = match compare_minecraft_versions(version, "1.21.9") {
        Some(Ordering::Less) => PackMetadataFormat::PackFormatOnly,
        Some(_) => PackMetadataFormat::FormatRange,
        None => return Err(ExportError::UnknownPackFormat(version.to_owned())),
    };
    Ok(ResolvedPackFormat {
        major: manual.major,
        minor: manual.minor,
        metadata,
    })
}

pub(crate) fn pack_metadata(
    pack_format: ResolvedPackFormat,
    description: &str,
) -> Result<Vec<u8>, ExportError> {
    let format_number = Value::from(pack_format.major);
    let format_version = Value::Array(vec![
        Value::from(pack_format.major),
        Value::from(pack_format.minor),
    ]);
    let mut pack = Map::new();
    pack.insert(
        "description".to_owned(),
        Value::String(description.to_owned()),
    );
    match pack_format.metadata {
        PackMetadataFormat::PackFormatOnly => {
            pack.insert("pack_format".to_owned(), format_number);
        }
        PackMetadataFormat::FormatRange => {
            // 1.21.9以降は範囲指定が必須。現在の単一version向けpackなので上下限を同値にする。
            pack.insert("min_format".to_owned(), format_version.clone());
            pack.insert("max_format".to_owned(), format_version);
            pack.insert("pack_format".to_owned(), format_number);
        }
    }
    let mut root = Map::new();
    root.insert("pack".to_owned(), Value::Object(pack));
    let mut bytes = serde_json::to_vec_pretty(&Value::Object(root))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_resource_pack_zip(
    file: &mut File,
    metadata: &[u8],
    language_entries: &[(String, Vec<u8>)],
) -> Result<(), ExportError> {
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut zip = ZipWriter::new(file);
    zip.start_file("pack.mcmeta", options)?;
    zip.write_all(metadata)?;
    for (path, bytes) in language_entries {
        zip.start_file(path, options)?;
        zip.write_all(bytes)?;
    }
    zip.finish()?;
    Ok(())
}

pub(crate) fn write_resource_pack_archive(
    output_path: &Path,
    metadata: &[u8],
    language_entries: &[(String, Vec<u8>)],
    overwrite: bool,
) -> Result<(), ExportError> {
    let expected_entries: Vec<_> = std::iter::once("pack.mcmeta".to_owned())
        .chain(language_entries.iter().map(|(path, _)| path.clone()))
        .collect();
    atomic_write_file(
        output_path,
        overwrite,
        |file| write_resource_pack_zip(file, metadata, language_entries),
        |path| validate_zip(path, &expected_entries),
    )
}

fn filename_component(value: &str) -> String {
    let mut component = String::with_capacity(value.len());
    for character in value.trim().chars() {
        if matches!(
            character,
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
        ) {
            component.push('_');
        } else if !character.is_control() {
            component.push(character);
        }
    }
    let component =
        component.trim_matches(|character: char| character.is_whitespace() || character == '.');
    if component.is_empty() {
        "mod".to_owned()
    } else {
        component.to_owned()
    }
}

fn source_jar_stem(source_jar_filename: &str) -> &str {
    let filename = source_jar_filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(source_jar_filename);
    if filename.len() >= 4 && filename[filename.len() - 4..].eq_ignore_ascii_case(".jar") {
        &filename[..filename.len() - 4]
    } else {
        filename
    }
}

fn resource_pack_mod_name<'a>(
    mods: &'a [ResourcePackMod],
    source_jar_filename: &'a str,
) -> &'a str {
    // JAR単位で配布するため、複数Modがあっても先頭を代表名にし、名前がなければJAR名を使う。
    mods.first()
        .and_then(|mod_info| mod_info.name.as_deref())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| source_jar_stem(source_jar_filename))
}
