use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::Value;

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// One archive member. The caller owns the archive and provides its member path and bytes.
#[derive(Clone, Copy, Debug)]
pub struct PatchouliArchiveFile<'a> {
    pub path: &'a str,
    pub bytes: &'a [u8],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatchouliWarning {
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatchouliBookConfig {
    pub path: String,
    pub is_data_declaration: bool,
    pub i18n: bool,
    pub name: Option<String>,
    pub landing_text: Option<String>,
    pub subtitle: Option<String>,
    /// Retains the remaining book settings for later pipeline stages.
    pub raw: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PatchouliDocumentKind {
    Category,
    Entry,
    Template,
}

#[derive(Clone, Debug)]
pub struct PatchouliDocument<'a> {
    pub path: String,
    pub locale: String,
    /// Path within the locale folder, such as `entries/machines/pump.json`.
    pub relative_path: String,
    pub kind: PatchouliDocumentKind,
    bytes: &'a [u8],
}

impl<'a> PatchouliDocument<'a> {
    /// Returns the archive member bytes used for parsing and document rebuilding.
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

#[derive(Clone, Debug)]
pub struct PatchouliBook<'a> {
    pub namespace: String,
    pub book_id: String,
    /// `true` means Patchouli resolves content strings through lang files; document
    /// translations must not be created for this book.
    pub i18n: bool,
    pub available_locales: Vec<String>,
    /// Paths to the English source documents (`en_us`).
    pub source_document_paths: Vec<String>,
    pub book_configs: Vec<PatchouliBookConfig>,
    pub documents: Vec<PatchouliDocument<'a>>,
}

#[derive(Clone, Debug, Default)]
pub struct PatchouliDiscovery<'a> {
    pub books: Vec<PatchouliBook<'a>>,
    pub warnings: Vec<PatchouliWarning>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatchouliEntry {
    pub document_path: String,
    pub pointer: String,
    pub source_text: String,
    pub key: String,
    /// The value at the same pointer in the target locale document, if present.
    pub target_text: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PatchouliExtraction {
    pub entries: Vec<PatchouliEntry>,
    pub warnings: Vec<PatchouliWarning>,
    pub skipped_for_i18n: bool,
}

#[derive(Clone, Debug)]
pub struct PatchouliRebuild {
    pub bytes: Vec<u8>,
    pub warnings: Vec<PatchouliWarning>,
}

#[derive(Default)]
struct BookBuilder<'a> {
    book_configs: Vec<PatchouliBookConfig>,
    documents: Vec<PatchouliDocument<'a>>,
}

/// Finds books and locale documents in archive members.
///
/// Book declarations are read from both `data` and `assets`. Legacy content under
/// `data/<namespace>/patchouli_books/<book>/<locale>/` is intentionally ignored.
pub fn discover_books<'a>(files: &[PatchouliArchiveFile<'a>]) -> PatchouliDiscovery<'a> {
    let mut sorted_files = files.to_vec();
    sorted_files.sort_by(|left, right| left.path.cmp(right.path));

    let mut builders: BTreeMap<(String, String), BookBuilder<'a>> = BTreeMap::new();
    let mut warnings = Vec::new();

    for file in sorted_files {
        if let Some((namespace, book_id, is_data_declaration)) = locate_book_config_path(file.path)
        {
            let builder = builders
                .entry((namespace.clone(), book_id.clone()))
                .or_default();
            match parse_book_config(file.path, file.bytes, is_data_declaration) {
                Ok(config) => builder.book_configs.push(config),
                Err(reason) => push_warning(&mut warnings, file.path, reason),
            }
        }

        if let Some((namespace, book_id, locale, relative_path, kind)) =
            locate_document_path(file.path)
        {
            builders
                .entry((namespace.clone(), book_id.clone()))
                .or_default()
                .documents
                .push(PatchouliDocument {
                    path: file.path.to_owned(),
                    locale,
                    relative_path,
                    kind,
                    bytes: file.bytes,
                });
        }
    }

    let books = builders
        .into_iter()
        .map(|((namespace, book_id), mut builder)| {
            builder.book_configs.sort_by(|left, right| {
                (!left.is_data_declaration, &left.path)
                    .cmp(&(!right.is_data_declaration, &right.path))
            });
            builder
                .documents
                .sort_by(|left, right| left.path.cmp(&right.path));

            let i18n = builder
                .book_configs
                .iter()
                .find(|config| config.is_data_declaration)
                .or_else(|| builder.book_configs.first())
                .is_some_and(|config| config.i18n);
            let available_locales: BTreeSet<_> = builder
                .documents
                .iter()
                .map(|document| document.locale.clone())
                .collect();
            let source_document_paths = builder
                .documents
                .iter()
                .filter(|document| document.locale.eq_ignore_ascii_case("en_us"))
                .map(|document| document.path.clone())
                .collect();

            PatchouliBook {
                namespace,
                book_id,
                i18n,
                available_locales: available_locales.into_iter().collect(),
                source_document_paths,
                book_configs: builder.book_configs,
                documents: builder.documents,
            }
        })
        .collect();

    PatchouliDiscovery { books, warnings }
}

/// Extracts display strings from source locale documents and joins any existing target strings.
pub fn extract_entries(
    book: &PatchouliBook<'_>,
    source_locale: &str,
    target_locale: &str,
) -> PatchouliExtraction {
    if book.i18n {
        return PatchouliExtraction {
            skipped_for_i18n: true,
            ..PatchouliExtraction::default()
        };
    }

    let source_documents: Vec<_> = book
        .documents
        .iter()
        .filter(|document| document.locale.eq_ignore_ascii_case(source_locale))
        .collect();
    let target_documents: HashMap<_, _> = book
        .documents
        .iter()
        .filter(|document| document.locale.eq_ignore_ascii_case(target_locale))
        .map(|document| (document.relative_path.as_str(), document))
        .collect();

    let mut result = PatchouliExtraction::default();
    for source_document in source_documents {
        let source_json = match parse_document(source_document, &mut result.warnings) {
            Some(json) => json,
            None => continue,
        };
        let strings = match collect_translatable_strings(source_document.kind, &source_json) {
            Ok(strings) => strings,
            Err(reason) => {
                push_warning(&mut result.warnings, &source_document.path, reason);
                continue;
            }
        };

        let target_json = target_documents
            .get(source_document.relative_path.as_str())
            .and_then(|document| {
                let json = parse_document(document, &mut result.warnings)?;
                match collect_translatable_strings(document.kind, &json) {
                    Ok(_) => Some(json),
                    Err(reason) => {
                        push_warning(&mut result.warnings, &document.path, reason);
                        None
                    }
                }
            });

        for (pointer, source_text) in strings {
            let target_text = target_json
                .as_ref()
                .and_then(|json| pointer_value(json, &pointer))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let key = format!(
                "patchouli:{}:{}:{}#{}",
                book.namespace, book.book_id, source_document.relative_path, pointer
            );
            result.entries.push(PatchouliEntry {
                document_path: source_document.path.clone(),
                pointer,
                source_text,
                key,
                target_text,
            });
        }
    }

    result
}

/// Returns the strings used as translation keys by an i18n book.
pub fn extract_i18n_keys(book: &PatchouliBook<'_>) -> (Vec<String>, Vec<PatchouliWarning>) {
    if !book.i18n {
        return (Vec::new(), Vec::new());
    }

    let mut keys = BTreeSet::new();
    let mut warnings = Vec::new();
    for config in &book.book_configs {
        for value in [&config.name, &config.landing_text, &config.subtitle]
            .into_iter()
            .flatten()
        {
            keys.insert(value.clone());
        }
    }
    for document in book
        .documents
        .iter()
        .filter(|document| document.locale.eq_ignore_ascii_case("en_us"))
    {
        let Some(json) = parse_document(document, &mut warnings) else {
            continue;
        };
        match collect_translatable_strings(document.kind, &json) {
            Ok(strings) => keys.extend(strings.into_iter().map(|(_, value)| value)),
            Err(reason) => push_warning(&mut warnings, &document.path, reason),
        }
    }
    (keys.into_iter().collect(), warnings)
}

/// Replaces only values addressed by valid JSON pointers. Invalid source JSON is returned
/// unchanged and reported through tracing; use `rebuild_document_with_warnings` to inspect it.
pub fn rebuild_document(
    source_json_bytes: &[u8],
    translations: &HashMap<String, String>,
) -> Vec<u8> {
    rebuild_document_with_warnings(source_json_bytes, translations).bytes
}

/// Warning-returning variant of `rebuild_document`.
pub fn rebuild_document_with_warnings(
    source_json_bytes: &[u8],
    translations: &HashMap<String, String>,
) -> PatchouliRebuild {
    let had_bom = source_json_bytes.starts_with(UTF8_BOM);
    let mut json = match parse_json(source_json_bytes) {
        Ok(json) => json,
        Err(reason) => {
            let mut warnings = Vec::new();
            push_warning(&mut warnings, "<document>", reason);
            return PatchouliRebuild {
                bytes: source_json_bytes.to_vec(),
                warnings,
            };
        }
    };

    for (pointer, text) in translations {
        if let Some(value) = pointer_value_mut(&mut json, pointer)
            && value.is_string()
        {
            *value = Value::String(text.clone());
        }
    }

    let mut bytes = match serde_json::to_vec(&json) {
        Ok(bytes) => bytes,
        Err(reason) => {
            let mut warnings = Vec::new();
            push_warning(
                &mut warnings,
                "<document>",
                format!("JSON serialization failed: {reason}"),
            );
            return PatchouliRebuild {
                bytes: source_json_bytes.to_vec(),
                warnings,
            };
        }
    };
    if had_bom && !bytes.starts_with(UTF8_BOM) {
        let mut with_bom = UTF8_BOM.to_vec();
        with_bom.append(&mut bytes);
        bytes = with_bom;
    }

    PatchouliRebuild {
        bytes,
        warnings: Vec::new(),
    }
}

/// Replaces the source locale folder in a Patchouli asset document path.
pub fn patchouli_target_document_path(
    source_path: &str,
    source_locale: &str,
    target_locale: &str,
) -> Option<String> {
    let mut segments: Vec<_> = source_path.split('/').collect();
    let assets_index = segments.iter().position(|segment| *segment == "assets")?;
    if segments.len() < assets_index + 7
        || segments[assets_index + 2] != "patchouli_books"
        || !segments[assets_index + 4].eq_ignore_ascii_case(source_locale)
        || !matches!(
            segments[assets_index + 5],
            "categories" | "entries" | "templates"
        )
    {
        return None;
    }
    segments[assets_index + 4] = target_locale;
    Some(segments.join("/"))
}

fn parse_book_config(
    path: &str,
    bytes: &[u8],
    is_data_declaration: bool,
) -> Result<PatchouliBookConfig, String> {
    let raw = parse_json(bytes).map_err(|reason| format!("invalid book.json: {reason}"))?;
    let object = raw
        .as_object()
        .ok_or_else(|| "book.json root must be an object".to_owned())?;
    let i18n = match object.get("i18n") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err("book.json `i18n` must be a boolean".to_owned()),
    };
    let name = optional_string(object, "name")?;
    let landing_text = optional_string(object, "landing_text")?;
    let subtitle = optional_string(object, "subtitle")?;

    Ok(PatchouliBookConfig {
        path: path.to_owned(),
        is_data_declaration,
        i18n,
        name,
        landing_text,
        subtitle,
        raw,
    })
}

fn optional_string(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<String>, String> {
    match object.get(field) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("book.json `{field}` must be a string")),
    }
}

fn locate_book_config_path(path: &str) -> Option<(String, String, bool)> {
    let segments = path_segments(path);
    for index in 0..segments.len().saturating_sub(4) {
        let root = segments[index];
        let is_data = root == "data";
        if !is_data && root != "assets" {
            continue;
        }
        if segments[index + 2] == "patchouli_books" && segments[index + 4] == "book.json" {
            let namespace = segments[index + 1];
            let book_id = segments[index + 3];
            if valid_path_component(namespace) && valid_path_component(book_id) {
                return Some((namespace.to_owned(), book_id.to_owned(), is_data));
            }
        }
    }
    None
}

fn locate_document_path(
    path: &str,
) -> Option<(String, String, String, String, PatchouliDocumentKind)> {
    let segments = path_segments(path);
    for index in 0..segments.len().saturating_sub(6) {
        if segments[index] != "assets" || segments[index + 2] != "patchouli_books" {
            continue;
        }
        let namespace = segments[index + 1];
        let book_id = segments[index + 3];
        let locale = segments[index + 4];
        let kind = match segments[index + 5] {
            "categories" => PatchouliDocumentKind::Category,
            "entries" => PatchouliDocumentKind::Entry,
            "templates" => PatchouliDocumentKind::Template,
            _ => continue,
        };
        if !valid_path_component(namespace)
            || !valid_path_component(book_id)
            || !valid_locale(locale)
            || !segments
                .last()
                .is_some_and(|segment| segment.ends_with(".json"))
            || segments.len() <= index + 6
        {
            continue;
        }
        let relative_path = segments[index + 5..].join("/");
        return Some((
            namespace.to_owned(),
            book_id.to_owned(),
            locale.to_owned(),
            relative_path,
            kind,
        ));
    }
    None
}

fn path_segments(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn valid_path_component(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".."
}

fn valid_locale(locale: &str) -> bool {
    let mut parts = locale.split('_');
    let Some(language) = parts.next() else {
        return false;
    };
    let regions: Vec<_> = parts.collect();
    (2..=3).contains(&language.len())
        && language.bytes().all(|byte| byte.is_ascii_alphabetic())
        && !regions.is_empty()
        && regions.iter().all(|region| {
            !region.is_empty() && region.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

fn parse_document<'a>(
    document: &PatchouliDocument<'a>,
    warnings: &mut Vec<PatchouliWarning>,
) -> Option<Value> {
    match parse_json(document.bytes) {
        Ok(json) => Some(json),
        Err(reason) => {
            push_warning(
                warnings,
                &document.path,
                format!("invalid document JSON: {reason}"),
            );
            None
        }
    }
}

fn collect_translatable_strings(
    kind: PatchouliDocumentKind,
    json: &Value,
) -> Result<Vec<(String, String)>, String> {
    let object = json
        .as_object()
        .ok_or_else(|| "document root must be an object".to_owned())?;
    let mut strings = Vec::new();

    match kind {
        PatchouliDocumentKind::Category => {
            collect_required_string(object, "name", "", &mut strings)?;
            collect_optional_string(object, "description", "", &mut strings)?;
        }
        PatchouliDocumentKind::Entry => {
            collect_required_string(object, "name", "", &mut strings)?;
            let pages = object
                .get("pages")
                .and_then(Value::as_array)
                .ok_or_else(|| "entry `pages` must be an array".to_owned())?;
            for (index, page) in pages.iter().enumerate() {
                let pointer = format!("/pages/{index}");
                match page {
                    Value::String(text) => strings.push((pointer, text.clone())),
                    Value::Object(page) => collect_page_strings(page, &pointer, &mut strings)?,
                    _ => {
                        return Err(format!(
                            "entry page at index {index} must be a string or object"
                        ));
                    }
                }
            }
        }
        PatchouliDocumentKind::Template => {
            let components = object
                .get("components")
                .and_then(Value::as_array)
                .ok_or_else(|| "template `components` must be an array".to_owned())?;
            for (index, component) in components.iter().enumerate() {
                let component = component.as_object().ok_or_else(|| {
                    format!("template component at index {index} must be an object")
                })?;
                let Some(component_type) = component.get("type") else {
                    return Err(format!("template component at index {index} has no `type`"));
                };
                let component_type = component_type.as_str().ok_or_else(|| {
                    format!("template component at index {index} has a non-string `type`")
                })?;
                if matches!(component_type, "patchouli:text" | "patchouli:header") {
                    let pointer = format!("/components/{index}");
                    let Some(Value::String(text)) = component.get("text") else {
                        return Err(format!(
                            "template text component at index {index} has no string `text`"
                        ));
                    };
                    // Template variables are preserved by the Patchouli placeholder profile.
                    strings.push((format!("{pointer}/text"), text.clone()));
                } else if component_type == "patchouli:tooltip" {
                    // Tooltip components render each tooltip string as user-facing text.
                    // Their array shape is documented by Patchouli's built-in template component.
                    let Some(Value::Array(tooltip)) = component.get("tooltip") else {
                        return Err(format!(
                            "template tooltip component at index {index} has no array `tooltip`"
                        ));
                    };
                    for (tooltip_index, value) in tooltip.iter().enumerate() {
                        let Some(text) = value.as_str() else {
                            return Err(format!(
                                "template tooltip at index {index} has a non-string value"
                            ));
                        };
                        strings.push((
                            format!("/components/{index}/tooltip/{tooltip_index}"),
                            text.to_owned(),
                        ));
                    }
                }
            }
        }
    }

    Ok(strings)
}

fn collect_page_strings(
    page: &serde_json::Map<String, Value>,
    pointer: &str,
    strings: &mut Vec<(String, String)>,
) -> Result<(), String> {
    let page_type = match page.get("type") {
        Some(Value::String(page_type)) => page_type.as_str(),
        Some(_) => return Err(format!("page at `{pointer}` has a non-string `type`")),
        None => return Err(format!("page at `{pointer}` has no `type`")),
    };

    // These are the display fields documented by Patchouli's built-in page types. Resource
    // locations, recipe/item identifiers, flags, anchors and structural fields stay untouched.
    let fields: &[&str] = match page_type {
        "patchouli:text"
        | "patchouli:image"
        | "patchouli:crafting"
        | "patchouli:smelting"
        | "patchouli:spotlight"
        | "patchouli:relations"
        | "patchouli:quest" => &["text", "title"],
        "patchouli:multiblock" | "patchouli:entity" => &["name", "text"],
        "patchouli:link" => &["text", "title", "link_text"],
        "patchouli:empty" => &[],
        _ => return Ok(()),
    };
    for field in fields {
        collect_optional_string(page, field, pointer, strings)?;
    }
    Ok(())
}

fn collect_required_string(
    object: &serde_json::Map<String, Value>,
    field: &str,
    parent_pointer: &str,
    strings: &mut Vec<(String, String)>,
) -> Result<(), String> {
    let Some(Value::String(text)) = object.get(field) else {
        return Err(format!("`{field}` must be a string"));
    };
    strings.push((join_pointer(parent_pointer, field), text.clone()));
    Ok(())
}

fn collect_optional_string(
    object: &serde_json::Map<String, Value>,
    field: &str,
    parent_pointer: &str,
    strings: &mut Vec<(String, String)>,
) -> Result<(), String> {
    match object.get(field) {
        None => Ok(()),
        Some(Value::String(text)) => {
            strings.push((join_pointer(parent_pointer, field), text.clone()));
            Ok(())
        }
        Some(_) => Err(format!("`{field}` at `{parent_pointer}` must be a string")),
    }
}

fn join_pointer(parent: &str, field: &str) -> String {
    format!("{parent}/{}", escape_pointer_segment(field))
}

fn escape_pointer_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn pointer_value<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    let mut current = root;
    for segment in pointer_segments(pointer)? {
        current = match current {
            Value::Object(object) => object.get(&segment)?,
            Value::Array(array) => array.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

fn pointer_value_mut<'a>(root: &'a mut Value, pointer: &str) -> Option<&'a mut Value> {
    let segments = pointer_segments(pointer)?;
    let mut current = root;
    for segment in segments {
        current = match current {
            Value::Object(object) => object.get_mut(&segment)?,
            Value::Array(array) => array.get_mut(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

fn pointer_segments(pointer: &str) -> Option<Vec<String>> {
    if pointer.is_empty() {
        return Some(Vec::new());
    }
    if !pointer.starts_with('/') {
        return None;
    }
    pointer[1..]
        .split('/')
        .map(|segment| {
            let mut decoded = String::with_capacity(segment.len());
            let mut chars = segment.chars();
            while let Some(character) = chars.next() {
                if character == '~' {
                    match chars.next()? {
                        '0' => decoded.push('~'),
                        '1' => decoded.push('/'),
                        _ => return None,
                    }
                } else {
                    decoded.push(character);
                }
            }
            Some(decoded)
        })
        .collect()
}

fn parse_json(bytes: &[u8]) -> Result<Value, String> {
    let bytes = bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes);
    let normalized = normalize_patchouli_json(bytes);
    serde_json::from_slice(&normalized).map_err(|error| error.to_string())
}

/// Gson's lenient reader accepts JavaScript-style comments and dangling commas. Remove those
/// lexical extensions while leaving quoted text and all JSON values unchanged for serde_json.
fn normalize_patchouli_json(bytes: &[u8]) -> Vec<u8> {
    let mut without_comments = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            without_comments.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            without_comments.push(byte);
            index += 1;
            continue;
        }

        let line_comment = byte == b'#' || (byte == b'/' && bytes.get(index + 1) == Some(&b'/'));
        let block_comment = byte == b'/' && bytes.get(index + 1) == Some(&b'*');
        if line_comment {
            while index < bytes.len() && !matches!(bytes[index], b'\n' | b'\r') {
                without_comments.push(b' ');
                index += 1;
            }
            continue;
        }
        if block_comment {
            without_comments.extend_from_slice(b"  ");
            index += 2;
            while index < bytes.len() {
                if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    without_comments.extend_from_slice(b"  ");
                    index += 2;
                    break;
                }
                let byte = bytes[index];
                without_comments.push(if matches!(byte, b'\n' | b'\r') {
                    byte
                } else {
                    b' '
                });
                index += 1;
            }
            continue;
        }

        without_comments.push(byte);
        index += 1;
    }

    let mut normalized = Vec::with_capacity(without_comments.len());
    index = 0;
    in_string = false;
    escaped = false;
    while index < without_comments.len() {
        let byte = without_comments[index];
        if in_string {
            normalized.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
        }
        if byte == b',' {
            let next_non_whitespace = without_comments[index + 1..]
                .iter()
                .position(|byte| !byte.is_ascii_whitespace())
                .map(|offset| without_comments[index + 1 + offset]);
            if matches!(next_non_whitespace, Some(b'}' | b']')) {
                normalized.push(b' ');
                index += 1;
                continue;
            }
        }
        normalized.push(byte);
        index += 1;
    }
    normalized
}

fn push_warning(warnings: &mut Vec<PatchouliWarning>, path: &str, reason: impl Into<String>) {
    let reason = reason.into();
    tracing::warn!(file = %path, reason = %reason, "Patchouli JSONをスキップします");
    warnings.push(PatchouliWarning {
        path: path.to_owned(),
        reason,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn archive_file<'a>(path: &'a str, bytes: &'a [u8]) -> PatchouliArchiveFile<'a> {
        PatchouliArchiveFile { path, bytes }
    }

    #[test]
    fn discovers_documents_extracts_display_fields_and_joins_existing_translation() {
        let files = [
            archive_file(
                "data/example/patchouli_books/guide/book.json",
                br#"{"name":"Guide","landing_text":"Welcome","subtitle":"Basics","i18n":false}"#,
            ),
            archive_file(
                "assets/example/patchouli_books/guide/en_us/categories/start.json",
                br#"{"name":"Getting Started","description":"Begin here","icon":"minecraft:book"}"#,
            ),
            archive_file(
                "assets/example/patchouli_books/guide/ja_jp/categories/start.json",
                r#"{"name":"はじめに","description":"ここから","icon":"minecraft:book"}"#.as_bytes(),
            ),
            archive_file(
                "assets/example/patchouli_books/guide/en_us/entries/start.json",
                br#"{"name":"First Steps","category":"example:start","icon":"minecraft:book","pages":[{"type":"patchouli:text","text":"Read this","title":"Welcome"},{"type":"patchouli:link","url":"https://example.com","link_text":"Open site"},{"type":"patchouli:spotlight","item":"minecraft:apple","text":"Eat it"},{"type":"patchouli:entity","entity":"minecraft:cow","name":"Cow","text":"Moo"},{"type":"custom:page","text":"ignore"}],"anchor":"chapter","sortnum":1}"#,
            ),
            archive_file(
                "assets/example/patchouli_books/guide/ja_jp/entries/start.json",
                r#"{"name":"最初の一歩","category":"example:start","icon":"minecraft:book","pages":[{"type":"patchouli:text","text":"読んでください","title":"ようこそ"},{"type":"patchouli:link","url":"https://example.com","link_text":"サイトを開く"},{"type":"patchouli:spotlight","item":"minecraft:apple","text":"食べて"},{"type":"patchouli:entity","entity":"minecraft:cow","name":"牛","text":"モー"},{"type":"custom:page","text":"ignore"}],"anchor":"chapter","sortnum":1}"#.as_bytes(),
            ),
            archive_file(
                "assets/example/patchouli_books/guide/en_us/templates/info.json",
                br##"{"components":[{"type":"patchouli:text","text":"Literal template text"},{"type":"patchouli:header","text":"Hello #name#"},{"type":"patchouli:item","item":"minecraft:book"}]}"##,
            ),
            archive_file(
                "data/example/patchouli_books/guide/en_us/entries/legacy.json",
                br#"{"name":"Ignored","pages":[]}"#,
            ),
        ];

        let discovery = discover_books(&files);
        assert!(discovery.warnings.is_empty());
        assert_eq!(discovery.books.len(), 1);
        let book = &discovery.books[0];
        assert_eq!(book.namespace, "example");
        assert_eq!(book.book_id, "guide");
        assert!(!book.i18n);
        assert_eq!(book.available_locales, ["en_us", "ja_jp"]);
        assert_eq!(book.source_document_paths.len(), 3);
        assert_eq!(book.book_configs[0].name.as_deref(), Some("Guide"));
        assert_eq!(
            book.book_configs[0].landing_text.as_deref(),
            Some("Welcome")
        );
        assert_eq!(book.book_configs[0].subtitle.as_deref(), Some("Basics"));

        let extraction = extract_entries(book, "en_us", "ja_jp");
        assert!(extraction.warnings.is_empty());
        assert!(!extraction.skipped_for_i18n);
        let entry_by_key: HashMap<_, _> = extraction
            .entries
            .iter()
            .map(|entry| (entry.key.as_str(), entry))
            .collect();
        assert_eq!(
            entry_by_key["patchouli:example:guide:categories/start.json#/name"].source_text,
            "Getting Started"
        );
        assert_eq!(
            entry_by_key["patchouli:example:guide:categories/start.json#/name"]
                .target_text
                .as_deref(),
            Some("はじめに")
        );
        assert_eq!(
            entry_by_key["patchouli:example:guide:categories/start.json#/description"]
                .target_text
                .as_deref(),
            Some("ここから")
        );
        assert_eq!(
            entry_by_key["patchouli:example:guide:entries/start.json#/pages/0/text"]
                .target_text
                .as_deref(),
            Some("読んでください")
        );
        assert_eq!(
            entry_by_key["patchouli:example:guide:entries/start.json#/pages/1/link_text"]
                .target_text
                .as_deref(),
            Some("サイトを開く")
        );
        assert_eq!(
            entry_by_key["patchouli:example:guide:entries/start.json#/pages/3/name"]
                .target_text
                .as_deref(),
            Some("牛")
        );
        assert!(
            entry_by_key
                .contains_key("patchouli:example:guide:templates/info.json#/components/0/text")
        );
        assert_eq!(
            entry_by_key["patchouli:example:guide:templates/info.json#/components/1/text"]
                .source_text,
            "Hello #name#"
        );
        assert!(
            !entry_by_key.contains_key("patchouli:example:guide:entries/start.json#/pages/2/item")
        );
        assert!(
            !entry_by_key.contains_key("patchouli:example:guide:entries/start.json#/pages/4/text")
        );
        assert!(
            extraction
                .entries
                .iter()
                .all(|entry| entry.key.starts_with("patchouli:example:guide:"))
        );
    }

    #[test]
    fn i18n_books_are_reported_without_document_translation_entries() {
        let files = [
            archive_file(
                "assets/example/patchouli_books/guide/book.json",
                br#"{"name":"key.name","landing_text":"key.landing","i18n":true}"#,
            ),
            archive_file(
                "assets/example/patchouli_books/guide/en_us/entries/start.json",
                br#"{"name":"key.entry","pages":["key.page"]}"#,
            ),
        ];
        let discovery = discover_books(&files);
        assert_eq!(discovery.books.len(), 1);
        assert!(discovery.books[0].i18n);

        let extraction = extract_entries(&discovery.books[0], "en_us", "ja_jp");
        assert!(extraction.skipped_for_i18n);
        assert!(extraction.entries.is_empty());
        assert!(extraction.warnings.is_empty());
    }

    #[test]
    fn rebuild_preserves_untranslated_values_and_json_member_order() {
        let source = br#"{"name":"Source","pages":[{"type":"patchouli:text","text":"Read","unknown":{"kept":true}}],"icon":"minecraft:book"}"#;
        let translations = HashMap::from([
            ("/name".to_owned(), "翻訳済み".to_owned()),
            ("/pages/0/text".to_owned(), "読む".to_owned()),
        ]);
        let rebuilt = rebuild_document(source, &translations);
        let rebuilt_json: Value = serde_json::from_slice(&rebuilt).unwrap();
        assert_eq!(
            rebuilt_json,
            json!({
                "name": "翻訳済み",
                "pages": [{
                    "type": "patchouli:text",
                    "text": "読む",
                    "unknown": {"kept": true}
                }],
                "icon": "minecraft:book"
            })
        );
        assert_eq!(
            rebuilt_json
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["name", "pages", "icon"]
        );
        assert_eq!(
            rebuilt_json["pages"][0]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["type", "text", "unknown"]
        );
    }

    #[test]
    fn accepts_comments_and_trailing_commas_and_returns_malformed_json_warning() {
        let relaxed = br#"{
            // comment
            "name": "A // literal",
            "pages": ["text",], /* block */
        }"#;
        let parsed = parse_json(relaxed).unwrap();
        assert_eq!(parsed["name"], "A // literal");
        assert_eq!(parsed["pages"][0], "text");

        let bad = rebuild_document_with_warnings(b"{", &HashMap::new());
        assert_eq!(bad.bytes, b"{");
        assert_eq!(bad.warnings.len(), 1);
    }

    #[test]
    fn target_path_replaces_only_the_locale_folder() {
        assert_eq!(
            patchouli_target_document_path(
                "assets/example/patchouli_books/guide/en_us/entries/first.json",
                "en_us",
                "ja_jp"
            )
            .as_deref(),
            Some("assets/example/patchouli_books/guide/ja_jp/entries/first.json")
        );
        assert_eq!(
            patchouli_target_document_path(
                "data/example/patchouli_books/guide/book.json",
                "en_us",
                "ja_jp"
            ),
            None
        );
    }
}
