use std::collections::BTreeMap;

use indexmap::IndexMap;

use crate::{
    ModpackError,
    model::{KubeJsOutputDocument, ModpackData, ModpackEntry, OutputFile},
};
use yc_language::is_empty_translation;

#[derive(Clone, Debug)]
pub(crate) struct KubeJsDocument {
    pub entries: IndexMap<String, String>,
}

pub(crate) fn parse_kubejs_document(
    path: &str,
    bytes: &[u8],
) -> Result<KubeJsDocument, ModpackError> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    serde_json::from_slice::<IndexMap<String, String>>(bytes)
        .map(|entries| KubeJsDocument { entries })
        .map_err(|source| ModpackError::Json {
            path: path.to_owned(),
            source,
        })
}

pub(crate) fn add_kubejs_entries(entries: &mut Vec<ModpackEntry>, document: &KubeJsOutputDocument) {
    for (key, source) in &document.source.entries {
        entries.push(ModpackEntry {
            key: format!("kubejs:{}#{key}", document.namespace),
            document_path: document.source_path.clone(),
            pointer: key.clone(),
            source_text: source.clone(),
            existing_translation: document
                .existing_translation
                .as_ref()
                .and_then(|translation| translation.entries.get(key).cloned()),
            translatable: !source.trim().is_empty(),
        });
    }
}

pub(crate) fn write_kubejs_outputs(
    data: &ModpackData,
    translations: &BTreeMap<String, String>,
) -> Result<Vec<OutputFile>, ModpackError> {
    data.kubejs_documents
        .iter()
        .map(|document| {
            let mut translated = IndexMap::new();
            for key in document.source.entries.keys() {
                let entry_key = format!("kubejs:{}#{key}", document.namespace);
                let translation = translations.get(&entry_key);
                if let Some(text) = translation {
                    if !is_empty_translation(text) {
                        translated.insert(key.clone(), text.clone());
                    }
                    continue;
                }
                let existing = document
                    .existing_translation
                    .as_ref()
                    .and_then(|translation| translation.entries.get(key))
                    .map(String::as_str)
                    .filter(|text| !is_empty_translation(text));
                if let Some(text) = existing {
                    translated.insert(key.clone(), text.to_owned());
                }
            }
            let mut bytes =
                serde_json::to_vec_pretty(&translated).map_err(|source| ModpackError::Json {
                    path: document.output_path.clone(),
                    source,
                })?;
            bytes.push(b'\n');
            Ok(OutputFile {
                path: document.output_path.clone(),
                bytes,
            })
        })
        .collect()
}
