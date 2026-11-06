use std::sync::{OnceLock, atomic::AtomicU8};

/// Minecraft 26.3のasset indexに含まれるpack.mcmeta由来の143言語。
mod minecraft;

pub use minecraft::{MinecraftLanguage, find_minecraft_language, minecraft_languages};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum UiLanguage {
    Ja,
    En,
    ZhHans,
    ZhHant,
    Ko,
}

impl UiLanguage {
    pub fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("ja") {
            Some(Self::Ja)
        } else if value.eq_ignore_ascii_case("en") {
            Some(Self::En)
        } else if value.eq_ignore_ascii_case("zh-hans") {
            Some(Self::ZhHans)
        } else if value.eq_ignore_ascii_case("zh-hant") {
            Some(Self::ZhHant)
        } else if value.eq_ignore_ascii_case("ko") {
            Some(Self::Ko)
        } else {
            None
        }
    }

    fn from_atomic(value: u8) -> Self {
        match value {
            0 => Self::Ja,
            2 => Self::ZhHans,
            3 => Self::ZhHant,
            4 => Self::Ko,
            _ => Self::En,
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::Ja => "ja",
            Self::En => "en",
            Self::ZhHans => "zh-Hans",
            Self::ZhHant => "zh-Hant",
            Self::Ko => "ko",
        }
    }
}

static UI_LANGUAGE: AtomicU8 = AtomicU8::new(UiLanguage::En as u8);

pub fn set_ui_language(language: UiLanguage) {
    UI_LANGUAGE.store(language as u8, std::sync::atomic::Ordering::Relaxed);
}

pub fn ui_language() -> UiLanguage {
    UiLanguage::from_atomic(UI_LANGUAGE.load(std::sync::atomic::Ordering::Relaxed))
}

type Catalog = std::collections::HashMap<String, std::collections::HashMap<String, String>>;

const MESSAGE_SOURCES: [&str; 14] = [
    include_str!("../messages/common.json"),
    include_str!("../messages/project.json"),
    include_str!("../messages/analysis.json"),
    include_str!("../messages/export.json"),
    include_str!("../messages/preview.json"),
    include_str!("../messages/ai.json"),
    include_str!("../messages/glossary.json"),
    include_str!("../messages/editor.json"),
    include_str!("../messages/modpack.json"),
    include_str!("../messages/settings.json"),
    include_str!("../messages/language.json"),
    include_str!("../messages/metadata.json"),
    include_str!("../messages/storage.json"),
    include_str!("../messages/packaging.json"),
];

static CATALOGS: [OnceLock<Catalog>; MESSAGE_SOURCES.len()] =
    [const { OnceLock::new() }; MESSAGE_SOURCES.len()];

fn catalog(index: usize) -> &'static Catalog {
    CATALOGS[index].get_or_init(|| {
        serde_json::from_str(MESSAGE_SOURCES[index])
            .expect("embedded locale message catalog must be valid JSON")
    })
}

fn namespace_index(namespace: &str) -> Option<usize> {
    match namespace {
        "common" => Some(0),
        "project" => Some(1),
        "analysis" => Some(2),
        "export" => Some(3),
        "preview" => Some(4),
        "ai" => Some(5),
        "glossary" => Some(6),
        "editor" => Some(7),
        "modpack" => Some(8),
        "settings" => Some(9),
        "language" => Some(10),
        "metadata" => Some(11),
        "storage" => Some(12),
        "packaging" => Some(13),
        _ => None,
    }
}

fn message(key: &str) -> String {
    let Some((namespace, _)) = key.split_once('.') else {
        return key.to_owned();
    };
    let Some(index) = namespace_index(namespace) else {
        return key.to_owned();
    };
    let Some(translations) = catalog(index).get(key) else {
        return key.to_owned();
    };
    translations
        .get(ui_language().code())
        .or_else(|| translations.get("en"))
        .or_else(|| translations.get("ja"))
        .map_or_else(|| key.to_owned(), Clone::clone)
}

#[doc(hidden)]
pub fn translate(key: &str, arguments: &[(&str, String)]) -> String {
    let template = message(key);
    let mut output = String::with_capacity(template.len());
    let mut remaining = template.as_str();
    while let Some(open) = remaining.find('{') {
        output.push_str(&remaining[..open]);
        let after_open = &remaining[open + 1..];
        let Some(close) = after_open.find('}') else {
            output.push_str(&remaining[open..]);
            return output;
        };
        let placeholder = &after_open[..close];
        if let Some((_, value)) = arguments.iter().find(|(name, _)| *name == placeholder) {
            output.push_str(value);
        } else {
            output.push('{');
            output.push_str(placeholder);
            output.push('}');
        }
        remaining = &after_open[close + 1..];
    }
    output.push_str(remaining);
    output
}

/// 現在のUI言語に合わせた自動プロジェクト名を返す。
pub fn auto_project_name(count: usize) -> String {
    match ui_language() {
        UiLanguage::Ja => format!("{count} 個の mod"),
        UiLanguage::En if count == 1 => format!("{count} mod"),
        UiLanguage::En => format!("{count} mods"),
        UiLanguage::ZhHans => format!("{count} 个模组"),
        UiLanguage::ZhHant => format!("{count} 個模組"),
        UiLanguage::Ko => format!("모드 {count}개"),
    }
}

/// 新旧UI言語で生成された自動プロジェクト名を判別する。
pub fn is_auto_project_name(name: &str) -> bool {
    let name = name.trim();
    [
        (" 個の mod", false),
        (" mod", false),
        (" mods", false),
        (" 个模组", false),
        (" 個模組", false),
        ("개", true),
    ]
    .into_iter()
    .any(|(suffix, korean)| {
        let count = if korean {
            name.strip_prefix("모드 ")
                .and_then(|prefix| prefix.strip_suffix(suffix))
        } else {
            name.strip_suffix(suffix)
        };
        count.is_some_and(|count| {
            !count.is_empty() && count.chars().all(|character| character.is_ascii_digit())
        })
    })
}

#[macro_export]
macro_rules! t {
    ($key:expr $(,)?) => {
        $crate::translate($key, &[])
    };
    ($key:expr, $($name:ident = $value:expr),+ $(,)?) => {{
        let arguments: &[(&str, String)] = &[
            $((stringify!($name), ::std::string::ToString::to_string(&$value))),+
        ];
        $crate::translate($key, arguments)
    }};
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        path::{Path, PathBuf},
        sync::Mutex,
    };

    use super::*;

    static LANGUAGE_TEST_LOCK: Mutex<()> = Mutex::new(());
    const REQUIRED_LANGUAGES: [&str; 5] = ["ja", "en", "zh-Hans", "zh-Hant", "ko"];

    fn all_catalogs() -> Vec<(&'static str, Catalog)> {
        MESSAGE_SOURCES
            .into_iter()
            .map(|source| {
                let catalog = serde_json::from_str(source).expect("catalog JSON should parse");
                (source, catalog)
            })
            .collect()
    }

    fn placeholders(message: &str) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        let mut remaining = message;
        while let Some(open) = remaining.find('{') {
            let after_open = &remaining[open + 1..];
            let Some(close) = after_open.find('}') else {
                break;
            };
            let name = &after_open[..close];
            if !name.is_empty()
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                result.insert(name.to_owned());
            }
            remaining = &after_open[close + 1..];
        }
        result
    }

    #[test]
    fn all_catalog_messages_are_complete_and_use_matching_placeholders() {
        for (_, catalog) in all_catalogs() {
            for (key, translations) in catalog {
                let expected = placeholders(
                    translations
                        .get("en")
                        .expect("English translation should exist"),
                );
                for language in REQUIRED_LANGUAGES {
                    let message = translations
                        .get(language)
                        .unwrap_or_else(|| panic!("{key} is missing {language}"));
                    assert!(!message.trim().is_empty(), "{key} is empty in {language}");
                    assert_eq!(placeholders(message), expected, "{key} in {language}");
                }
            }
        }
    }

    #[test]
    fn english_and_chinese_korean_catalogs_do_not_contain_kana() {
        for (_, catalog) in all_catalogs() {
            for (key, translations) in catalog {
                for language in ["en", "zh-Hans", "zh-Hant", "ko"] {
                    let message = translations
                        .get(language)
                        .unwrap_or_else(|| panic!("{key} is missing {language}"));
                    assert!(
                        !message.chars().any(is_kana),
                        "{key} contains kana in {language}: {message}"
                    );
                }
            }
        }
    }

    fn is_kana(character: char) -> bool {
        matches!(
            character as u32,
            0x3040..=0x30ff | 0x31f0..=0x31ff | 0xff65..=0xff9f
        )
    }

    #[test]
    fn minecraft_language_table_has_expected_entries_and_case_insensitive_lookup() {
        assert_eq!(minecraft_languages().len(), 143);
        assert_eq!(find_minecraft_language("en_us").unwrap().name, "English");
        assert_eq!(find_minecraft_language("JA_JP").unwrap().code, "ja_jp");
        assert!(find_minecraft_language("not_a_locale").is_none());
    }

    #[test]
    fn t_macro_interpolates_values_without_reinterpreting_inserted_braces() {
        let key = "missing {count} {name}";
        let inserted = "{name} stays literal";
        assert_eq!(
            t!(key, count = 3, name = inserted),
            "missing 3 {name} stays literal"
        );
    }

    #[test]
    fn ui_language_can_be_changed_and_auto_names_are_recognized_in_every_language() {
        let _guard = LANGUAGE_TEST_LOCK.lock().expect("language test lock");
        let previous = ui_language();
        for (language, expected) in [
            (UiLanguage::Ja, "3 個の mod"),
            (UiLanguage::En, "3 mods"),
            (UiLanguage::ZhHans, "3 个模组"),
            (UiLanguage::ZhHant, "3 個模組"),
            (UiLanguage::Ko, "모드 3개"),
        ] {
            set_ui_language(language);
            assert_eq!(auto_project_name(3), expected);
            assert!(is_auto_project_name(expected));
        }
        assert!(is_auto_project_name("12 個の mod"));
        assert!(is_auto_project_name("1 mod"));
        assert!(is_auto_project_name("1 mods"));
        assert!(!is_auto_project_name("mod 3"));
        assert_eq!(UiLanguage::parse("ZH-hant"), Some(UiLanguage::ZhHant));
        assert_eq!(UiLanguage::parse("fr"), None);
        set_ui_language(previous);
    }

    #[test]
    fn workspace_literal_translation_keys_exist_in_a_catalog() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("locale crate should be inside the workspace");
        let keys: BTreeMap<String, PathBuf> = rust_files(workspace)
            .into_iter()
            .flat_map(|path| {
                let source = fs::read_to_string(&path).expect("Rust source should be UTF-8");
                literal_translation_keys(&source)
                    .into_iter()
                    .map(move |key| (key, path.clone()))
            })
            .collect();
        for (key, path) in keys {
            let (namespace, _) = key
                .split_once('.')
                .unwrap_or_else(|| panic!("translation key has no namespace: {key} in {path:?}"));
            let index = namespace_index(namespace)
                .unwrap_or_else(|| panic!("unknown translation namespace in {key} ({path:?})"));
            assert!(
                catalog(index).contains_key(&key),
                "translation key {key} in {path:?} is missing from its catalog"
            );
        }
    }

    fn rust_files(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if !matches!(
                        path.file_name().and_then(|name| name.to_str()),
                        Some("target" | "node_modules" | ".git")
                    ) {
                        pending.push(path);
                    }
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    files.push(path);
                }
            }
        }
        files
    }

    fn literal_translation_keys(source: &str) -> Vec<String> {
        let mut keys = Vec::new();
        let bytes = source.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index..].starts_with(b"//") {
                index = bytes[index..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |newline| index + newline + 1);
                continue;
            }
            if bytes[index..].starts_with(b"/*") {
                index = skip_block_comment(bytes, index);
                continue;
            }
            if let Some(end) = raw_string_end(bytes, index) {
                index = end;
                continue;
            }
            if bytes[index] == b'"' {
                index = skip_quoted_string(bytes, index);
                continue;
            }
            if bytes[index] == b'b' && bytes.get(index + 1) == Some(&b'"') {
                index = skip_quoted_string(bytes, index + 1);
                continue;
            }

            let is_translation_macro = bytes[index..].starts_with(b"t!(")
                && (index == 0
                    || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_'));
            if is_translation_macro {
                let mut key_start = index + 3;
                while bytes.get(key_start).is_some_and(u8::is_ascii_whitespace) {
                    key_start += 1;
                }
                if bytes.get(key_start) == Some(&b'"') {
                    let key_end = skip_quoted_string(bytes, key_start);
                    if key_end > key_start + 1 {
                        keys.push(source[key_start + 1..key_end - 1].to_owned());
                    }
                    index = key_end;
                    continue;
                }
            }
            index += 1;
        }
        keys
    }

    fn skip_quoted_string(bytes: &[u8], quote: usize) -> usize {
        let mut index = quote + 1;
        while index < bytes.len() {
            match bytes[index] {
                b'\\' => index = (index + 2).min(bytes.len()),
                b'"' => return index + 1,
                _ => index += 1,
            }
        }
        bytes.len()
    }

    fn skip_block_comment(bytes: &[u8], start: usize) -> usize {
        let mut depth = 1;
        let mut index = start + 2;
        while index < bytes.len() {
            if bytes[index..].starts_with(b"/*") {
                depth += 1;
                index += 2;
            } else if bytes[index..].starts_with(b"*/") {
                depth -= 1;
                index += 2;
                if depth == 0 {
                    return index;
                }
            } else {
                index += 1;
            }
        }
        bytes.len()
    }

    fn raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
        let marker = if bytes.get(start) == Some(&b'r') {
            start
        } else if bytes.get(start..start + 2) == Some(b"br") {
            start + 1
        } else {
            return None;
        };
        if start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            return None;
        }
        let mut quote = marker + 1;
        while bytes.get(quote) == Some(&b'#') {
            quote += 1;
        }
        if bytes.get(quote) != Some(&b'"') {
            return None;
        }
        let hashes = quote - marker - 1;
        let mut index = quote + 1;
        while index < bytes.len() {
            if bytes[index] == b'"'
                && bytes
                    .get(index + 1..index + 1 + hashes)
                    .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
            {
                return Some(index + 1 + hashes);
            }
            index += 1;
        }
        Some(bytes.len())
    }
}
