use std::cmp::Ordering;

use serde::Serializer;
use serde::ser::SerializeMap;

use crate::discovery::{file_priority, normalize_locale};
use crate::{
    DiscoveredLanguageFile, LanguageError, LanguageFileFormat, TranslationEntry, WriterOptions,
};

/// 空欄の訳はMinecraftで何も表示されないため、未翻訳として扱う。
pub fn is_empty_translation(text: &str) -> bool {
    text.trim().is_empty()
}

/// 1つのnamespaceを2スペース字下げの決定的なUTF-8 JSONとして書き出す。
pub fn write_json_language(
    namespace: &str,
    entries: &[TranslationEntry],
    _options: WriterOptions,
) -> Result<Vec<u8>, LanguageError> {
    write_json_language_inner(namespace, entries, false)
}

fn write_json_language_inner(
    namespace: &str,
    entries: &[TranslationEntry],
    include_empty: bool,
) -> Result<Vec<u8>, LanguageError> {
    let entries = export_entries(namespace, entries, include_empty);
    let mut output = Vec::new();
    {
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
        let mut serializer = serde_json::Serializer::with_formatter(&mut output, formatter);
        let mut object = serializer.serialize_map(Some(entries.len()))?;
        for entry in entries {
            object.serialize_entry(&entry.key, &entry.target_text)?;
        }
        object.end()?;
    }
    output.push(b'\n');
    Ok(output)
}

/// 1つのnamespaceをlegacyのkey=value形式で書き出す。
pub fn write_legacy_language(
    namespace: &str,
    entries: &[TranslationEntry],
    options: WriterOptions,
) -> Vec<u8> {
    write_legacy_language_inner(namespace, entries, options, false)
}

fn write_legacy_language_inner(
    namespace: &str,
    entries: &[TranslationEntry],
    options: WriterOptions,
    include_empty: bool,
) -> Vec<u8> {
    let entries = export_entries(namespace, entries, include_empty);
    let mut output = String::new();
    let parse_escapes = options.legacy_parse_escapes
        || entries.iter().any(|entry| {
            // Vanillaは先頭の#をコメントとして扱い、最初の=でキーと値を分けるため、
            // そのまま表せないキーではForgeのPropertiesモードを有効にする。
            entry.key.starts_with('#')
                || entry.key.contains('=')
                || entry
                    .key
                    .chars()
                    .chain(entry.target_text.chars())
                    .any(|character| matches!(character, '\n' | '\r'))
        });

    if parse_escapes {
        // Forge 1.12.xはこのコメントがある場合だけJava Propertiesで読む。
        // 通常形式ではバックスラッシュを加工せず、Vanillaにそのまま表示させる。
        // https://github.com/MinecraftForge/MinecraftForge/blob/1.12.x/src/main/java/net/minecraftforge/fml/common/FMLCommonHandler.java#L712-L752
        output.push_str("#PARSE_ESCAPES\n");
    }

    for entry in entries {
        if parse_escapes {
            output.push_str(&escape_java_properties_component(&entry.key, true));
        } else {
            output.push_str(&entry.key);
        }
        output.push('=');
        if parse_escapes {
            output.push_str(&escape_java_properties_component(&entry.target_text, false));
        } else {
            output.push_str(&entry.target_text);
        }
        output.push('\n');
    }

    output.into_bytes()
}

/// Formatを指定してJSONまたはlegacy言語ファイルを書き出す。
pub fn write_language_file(
    namespace: &str,
    entries: &[TranslationEntry],
    format: LanguageFileFormat,
    options: WriterOptions,
) -> Result<Vec<u8>, LanguageError> {
    match format {
        LanguageFileFormat::Json => write_json_language(namespace, entries, options),
        LanguageFileFormat::LegacyLang => Ok(write_legacy_language(namespace, entries, options)),
    }
}

/// en_usを翻訳先にする場合、原本にある空値のキーも維持して書き出す。
pub fn write_language_file_preserving_empty(
    namespace: &str,
    entries: &[TranslationEntry],
    format: LanguageFileFormat,
    options: WriterOptions,
) -> Result<Vec<u8>, LanguageError> {
    match format {
        LanguageFileFormat::Json => write_json_language_inner(namespace, entries, true),
        LanguageFileFormat::LegacyLang => Ok(write_legacy_language_inner(
            namespace, entries, options, true,
        )),
    }
}

/// Language File / Modified JAR向けのlocale filenameを選ぶ。
/// 原文と同じ形式の翻訳先ファイルがあればその綴りを優先し、なければ原文の表記を使う。
pub fn language_export_filename(
    files: &[DiscoveredLanguageFile],
    namespace: &str,
    source_locale: &str,
    target_locale: &str,
) -> Option<String> {
    let source = language_export_source_file(files, namespace, source_locale, target_locale)?;
    target_filename_from_source(source, target_locale)
}

/// 書き出し時に形式とlocaleの大文字小文字を引き継ぐ既存ファイルを返す。
pub fn language_export_source_file<'a>(
    files: &'a [DiscoveredLanguageFile],
    namespace: &str,
    source_locale: &str,
    target_locale: &str,
) -> Option<&'a DiscoveredLanguageFile> {
    let mut namespace_files: Vec<_> = files
        .iter()
        .filter(|file| file.namespace == namespace)
        .collect();
    namespace_files.sort_by(|left, right| file_priority(left).cmp(&file_priority(right)));

    let source = namespace_files
        .iter()
        .copied()
        .find(|file| file.normalized_locale == normalize_locale(source_locale))
        // Modごとに選択localeがない場合も、翻訳先以外の原文localeの形式を使う。
        .or_else(|| {
            namespace_files
                .iter()
                .copied()
                .find(|file| file.normalized_locale != normalize_locale(target_locale))
        })
        // 翻訳先localeしかないnamespaceも、そのファイルの形式で書き出せる。
        .or_else(|| namespace_files.first().copied())?;

    // 古い.langが残っていても、JSON原文のmodにはゲームが読むJSONを出力する。
    namespace_files
        .iter()
        .copied()
        .find(|file| {
            file.normalized_locale == normalize_locale(target_locale)
                && file.format == source.format
        })
        .or(Some(source))
}

fn target_filename_from_source(
    source: &DiscoveredLanguageFile,
    target_locale: &str,
) -> Option<String> {
    let target_locale = normalize_locale(target_locale);
    if source.normalized_locale == target_locale {
        return Some(source.file_name.clone());
    }
    let (stem, extension) = source.file_name.rsplit_once('.')?;
    let target_spelling = target_locale
        .split_once('_')
        .map(|(language, region)| {
            let use_upper_region = stem
                .rsplit_once('_')
                .map(|(_, region)| {
                    !region.is_empty() && region.chars().all(|ch| !ch.is_ascii_lowercase())
                })
                .unwrap_or_else(|| stem.chars().any(|character| character.is_ascii_uppercase()));
            if use_upper_region {
                format!(
                    "{}_{}",
                    language.to_ascii_lowercase(),
                    region.to_ascii_uppercase()
                )
            } else {
                target_locale.clone()
            }
        })
        .unwrap_or(target_locale);
    Some(format!("{target_spelling}.{extension}"))
}

/// Minecraft versionを数値の各要素として比較し、不明な表記ならNoneを返す。
pub fn compare_minecraft_versions(left: &str, right: &str) -> Option<Ordering> {
    let left = parse_version(left)?;
    let right = parse_version(right)?;
    let component_count = left.len().max(right.len());

    for index in 0..component_count {
        match left
            .get(index)
            .copied()
            .unwrap_or_default()
            .cmp(&right.get(index).copied().unwrap_or_default())
        {
            Ordering::Equal => {}
            ordering => return Some(ordering),
        }
    }

    Some(Ordering::Equal)
}

/// Resource PackのMinecraft version ruleに合う言語ファイル名を返す。
pub fn resource_pack_language_filename(
    version: Option<&str>,
    target_locale: &str,
) -> Option<String> {
    let version = version?;
    if compare_minecraft_versions(version, "1.13")? != Ordering::Less {
        Some(format!("{}.json", normalize_locale(target_locale)))
    } else if compare_minecraft_versions(version, "1.11")? != Ordering::Less {
        Some(format!("{}.lang", normalize_locale(target_locale)))
    } else if compare_minecraft_versions(version, "1.6.1")? != Ordering::Less {
        let locale = normalize_locale(target_locale);
        let locale = locale
            .split_once('_')
            .map(|(language, region)| {
                format!(
                    "{}_{}",
                    language.to_ascii_lowercase(),
                    region.to_ascii_uppercase()
                )
            })
            .unwrap_or(locale);
        Some(format!("{locale}.lang"))
    } else {
        None
    }
}

fn export_entries<'a>(
    namespace: &str,
    entries: &'a [TranslationEntry],
    include_empty: bool,
) -> Vec<&'a TranslationEntry> {
    let mut selected: Vec<_> = entries
        .iter()
        .filter(|entry| {
            entry.namespace == namespace
                && (include_empty || !is_empty_translation(&entry.target_text))
        })
        .collect();
    // 判定だけtrimし、翻訳済みテキストの意図した前後の空白は保持する。
    selected.sort_by(|left, right| {
        left.sort_index
            .cmp(&right.sort_index)
            .then_with(|| left.key.cmp(&right.key))
    });
    selected
}

fn escape_java_properties_component(value: &str, is_key: bool) -> String {
    let mut output = String::with_capacity(value.len());
    for (index, character) in value.chars().enumerate() {
        match character {
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{000c}' => output.push_str("\\f"),
            ' ' if is_key || index == 0 => output.push_str("\\ "),
            '=' | ':' if is_key => {
                output.push('\\');
                output.push(character);
            }
            '#' | '!' if is_key && index == 0 => {
                output.push('\\');
                output.push(character);
            }
            character => output.push(character),
        }
    }
    output
}

fn parse_version(version: &str) -> Option<Vec<u64>> {
    let version = version.trim();
    if version.is_empty() {
        return None;
    }

    version
        .split('.')
        .map(|component| {
            if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
                None
            } else {
                component.parse().ok()
            }
        })
        .collect()
}
