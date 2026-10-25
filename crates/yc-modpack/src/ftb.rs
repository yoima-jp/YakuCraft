use std::collections::{BTreeMap, HashMap};

use crate::{
    ModpackError,
    model::{FtbDocument, ModpackData, ModpackEntry, OutputFile},
    snbt::{SnbtDocument, SnbtValue, parse_snbt, write_snbt},
};
use yc_language::is_empty_translation;

pub(crate) fn parse_ftb_document(path: &str, bytes: &[u8]) -> Result<SnbtDocument, ModpackError> {
    let text = std::str::from_utf8(bytes).map_err(|error| ModpackError::Snbt {
        path: path.to_owned(),
        offset: error.valid_up_to(),
        message: "file is not valid UTF-8".to_owned(),
    })?;
    parse_snbt(text).map_err(|(offset, message)| ModpackError::Snbt {
        path: path.to_owned(),
        offset,
        message,
    })
}

pub(crate) fn add_ftb_entries(entries: &mut Vec<ModpackEntry>, document: &FtbDocument) {
    let existing_by_key: HashMap<_, _> = document
        .existing_translation
        .as_ref()
        .map(|translation| {
            translation
                .entries
                .iter()
                .map(|(key, value)| (key.as_str(), value))
                .collect()
        })
        .unwrap_or_default();
    for (key, source_value) in &document.source.entries {
        let existing_value = existing_by_key.get(key.as_str()).copied();
        match source_value {
            SnbtValue::String(source) => entries.push(make_entry(
                &document.source_path,
                &document.key_path,
                key,
                None,
                source,
                existing_value.and_then(|value| match value {
                    SnbtValue::String(text) => Some(text.clone()),
                    SnbtValue::StringArray(_) => None,
                }),
            )),
            SnbtValue::StringArray(source_values) => {
                for (index, source) in source_values.iter().enumerate() {
                    let existing = existing_value.and_then(|value| match value {
                        SnbtValue::StringArray(values) => values.get(index).cloned(),
                        SnbtValue::String(_) => None,
                    });
                    entries.push(make_entry(
                        &document.source_path,
                        &document.key_path,
                        key,
                        Some(index),
                        source,
                        existing,
                    ));
                }
            }
        }
    }
}

fn make_entry(
    document_path: &str,
    key_path: &str,
    lang_key: &str,
    index: Option<usize>,
    source: &str,
    existing_translation: Option<String>,
) -> ModpackEntry {
    let pointer = match index {
        Some(index) => format!("{lang_key}[{index}]"),
        None => lang_key.to_owned(),
    };
    ModpackEntry {
        key: format!("ftbquests:{key_path}#{pointer}"),
        document_path: document_path.to_owned(),
        pointer,
        source_text: source.to_owned(),
        existing_translation,
        translatable: is_translatable(source),
    }
}

pub(crate) fn is_translatable(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "{@pagebreak}" {
        return false;
    }
    trimmed
        .strip_prefix("{image:")
        .and_then(|image| image.strip_suffix('}'))
        .is_none_or(|image| image.contains('{') || image.contains('}'))
}

pub(crate) fn write_ftb_outputs(
    data: &ModpackData,
    translations: &BTreeMap<String, String>,
) -> Result<Vec<OutputFile>, ModpackError> {
    data.ftb_documents
        .iter()
        .map(|document| {
            let mut translated = document.source.clone();
            let existing_by_key: HashMap<_, _> = document
                .existing_translation
                .as_ref()
                .map(|translation| {
                    translation
                        .entries
                        .iter()
                        .map(|(key, value)| (key.as_str(), value))
                        .collect()
                })
                .unwrap_or_default();
            for (key, source_value) in &mut translated.entries {
                let existing_value = existing_by_key.get(key.as_str()).copied();
                match source_value {
                    SnbtValue::String(source) => {
                        let entry_key = format!("ftbquests:{}#{key}", document.key_path);
                        let existing = existing_value.and_then(|value| match value {
                            SnbtValue::String(text) => Some(text.as_str()),
                            SnbtValue::StringArray(_) => None,
                        });
                        *source = choose_translation(
                            &entry_key,
                            source,
                            existing,
                            translations,
                            is_translatable(source),
                        );
                    }
                    SnbtValue::StringArray(values) => {
                        for (index, source) in values.iter_mut().enumerate() {
                            let entry_key =
                                format!("ftbquests:{}#{key}[{index}]", document.key_path);
                            let existing = existing_value.and_then(|value| match value {
                                SnbtValue::StringArray(values) => {
                                    values.get(index).map(String::as_str)
                                }
                                SnbtValue::String(_) => None,
                            });
                            *source = choose_translation(
                                &entry_key,
                                source,
                                existing,
                                translations,
                                is_translatable(source),
                            );
                        }
                    }
                }
            }
            Ok(OutputFile {
                path: document.output_path.clone(),
                bytes: write_snbt(&translated).into_bytes(),
            })
        })
        .collect()
}

pub(crate) fn choose_translation(
    key: &str,
    source: &str,
    existing: Option<&str>,
    translations: &BTreeMap<String, String>,
    translatable: bool,
) -> String {
    let translated = translations
        .get(key)
        .map(String::as_str)
        .filter(|text| !is_empty_translation(text));
    let existing = existing.filter(|text| !is_empty_translation(text));
    if translatable {
        translated.or(existing).unwrap_or(source).to_owned()
    } else {
        existing.unwrap_or(source).to_owned()
    }
}
