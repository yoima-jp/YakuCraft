use std::collections::HashSet;

use encoding_rs::{SHIFT_JIS, WINDOWS_1252};
use indexmap::IndexMap;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::{
    DiscoveredLanguageFile, LanguageError, LanguageFileFormat, LanguageParseWarning,
    ParsedLanguageContent, ParsedLanguageFile, ParsedLanguageValue,
};

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

struct JsonLanguageObject {
    entries: IndexMap<String, Value>,
    duplicate_keys: Vec<String>,
}

impl<'de> Deserialize<'de> for JsonLanguageObject {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = JsonLanguageObject;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object mapping translation keys to values")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut entries = IndexMap::new();
                let mut duplicate_keys = Vec::new();
                let mut seen_duplicates = HashSet::new();

                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    if entries.contains_key(&key) && seen_duplicates.insert(key.clone()) {
                        duplicate_keys.push(key.clone());
                    }
                    // 重複キーは最初の位置を保ち、値だけ最後の定義へ置き換える。
                    entries.insert(key, value);
                }

                Ok(JsonLanguageObject {
                    entries,
                    duplicate_keys,
                })
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

/// UTF-8 JSONを順序を保って解析し、文字列以外の値を不正な原文として保持する。
pub fn parse_json_language(bytes: &[u8]) -> Result<ParsedLanguageContent, LanguageError> {
    let bytes = bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes);
    let object = serde_json::from_slice::<JsonLanguageObject>(bytes)?;
    let mut warnings = Vec::with_capacity(object.duplicate_keys.len());

    for key in object.duplicate_keys {
        tracing::warn!(key = %key, "言語JSONに重複キーがあります。最後の値を使用します");
        warnings.push(LanguageParseWarning::DuplicateKey { key });
    }

    let entries = object
        .entries
        .into_iter()
        .map(|(key, value)| {
            let parsed = match value {
                Value::String(text) => ParsedLanguageValue {
                    text: Some(text),
                    malformed_source: false,
                },
                _ => ParsedLanguageValue {
                    text: None,
                    malformed_source: true,
                },
            };
            (key, parsed)
        })
        .collect();

    Ok(ParsedLanguageContent {
        entries,
        warnings,
        legacy_parse_escapes: false,
    })
}

/// Legacy .langをUTF-8で読み、失敗時はWindows-1252として解釈する。
pub fn parse_legacy_language(bytes: &[u8]) -> ParsedLanguageContent {
    parse_legacy_language_for_locale(bytes, None)
}

fn parse_legacy_language_for_locale(bytes: &[u8], locale: Option<&str>) -> ParsedLanguageContent {
    let (text, encoding) = match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_owned(), None),
        Err(_)
            if locale.is_some_and(|locale| {
                locale
                    .get(..3)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("ja_"))
            }) =>
        {
            // 日本語の旧形式はShift_JISが多いため、誤った西欧文字への置換を避けて先に試す。
            let (decoded, _, had_errors) = SHIFT_JIS.decode(bytes);
            if had_errors {
                let (fallback, _, _) = WINDOWS_1252.decode(bytes);
                (fallback.into_owned(), Some("windows-1252"))
            } else {
                (decoded.into_owned(), Some("shift_jis"))
            }
        }
        Err(_) => {
            let (decoded, _, _) = WINDOWS_1252.decode(bytes);
            (decoded.into_owned(), Some("windows-1252"))
        }
    };
    let normalized_text = text.replace("\r\n", "\n").replace('\r', "\n");
    let legacy_parse_escapes = has_parse_escapes_marker(&normalized_text);
    let text = normalized_text
        .strip_prefix('\u{feff}')
        .unwrap_or(&normalized_text);
    let mut entries = IndexMap::new();
    let mut warnings = Vec::new();
    let mut duplicate_keys = HashSet::new();

    if let Some(encoding) = encoding {
        tracing::warn!(
            encoding = encoding,
            "legacy言語ファイルをUTF-8以外の文字コードで読み込みました"
        );
        warnings.push(LanguageParseWarning::EncodingFallback {
            encoding: encoding.to_owned(),
        });
    }

    let parsed_entries = if legacy_parse_escapes {
        parse_java_properties(text)
    } else {
        parse_vanilla_legacy_lines(text)
    };
    for (key, value) in parsed_entries {
        if entries.contains_key(&key) && duplicate_keys.insert(key.clone()) {
            tracing::warn!(key = %key, "legacy言語ファイルに重複キーがあります。最後の値を使用します");
            warnings.push(LanguageParseWarning::DuplicateKey { key: key.clone() });
        }
        entries.insert(
            key,
            ParsedLanguageValue {
                text: Some(value),
                malformed_source: false,
            },
        );
    }

    ParsedLanguageContent {
        entries,
        warnings,
        legacy_parse_escapes,
    }
}

/// Forge hookと同じ条件で、Vanillaの読み込みを迂回するマーカーを判定する。
pub fn legacy_language_uses_parse_escapes(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let normalized_text = text.replace("\r\n", "\n").replace('\r', "\n");
    has_parse_escapes_marker(&normalized_text)
}

fn has_parse_escapes_marker(text: &str) -> bool {
    // Forge 1.12.xは行頭の'#'以降をtrimし、PARSE_ESCAPESと一致した場合にだけ
    // java.util.Properties.loadへ切り替える。
    // https://github.com/MinecraftForge/MinecraftForge/blob/1.12.x/src/main/java/net/minecraftforge/fml/common/FMLCommonHandler.java#L712-L752
    text.lines().any(|line| {
        line.strip_prefix('#')
            .is_some_and(|rest| rest.trim() == "PARSE_ESCAPES")
    })
}

fn parse_vanilla_legacy_lines(text: &str) -> Vec<(String, String)> {
    // Forgeはマーカーのないファイルを変更せずVanillaへ渡す。通常形式は行単位の
    // key=valueであり、値のバックスラッシュは通常の文字として扱う。
    // https://github.com/MinecraftForge/MinecraftForge/blob/1.12.x/src/main/java/net/minecraftforge/fml/common/FMLCommonHandler.java#L712-L752
    // https://docs.minecraftforge.net/en/1.12.x/concepts/internationalization/
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            line.split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

pub fn parse_java_properties(text: &str) -> Vec<(String, String)> {
    parse_java_properties_with_raw_keys(text)
        .into_iter()
        .map(|(key, _, value)| (key, value))
        .collect()
}

/// Propertiesとして解釈したキーと値に加え、元ファイルのキー表記を返す。
/// キーが物理行をまたぐ場合は、元の表記を復元できないためraw_keyをNoneにする。
pub fn parse_java_properties_with_raw_keys(text: &str) -> Vec<(String, Option<String>, String)> {
    // Forgeが使う文字ストリーム版Properties.loadに合わせ、Unicode、区切り文字のescape、
    // 複数行の継続を扱う。
    // https://docs.oracle.com/javase/8/docs/api/java/util/Properties.html#load-java.io.Reader-
    java_properties_logical_lines_with_key_completeness(text)
        .into_iter()
        .filter_map(|(line, raw_key_complete)| {
            let chars: Vec<_> = line.chars().collect();
            let key_offset = chars
                .iter()
                .take_while(|character| matches!(character, ' ' | '\t' | '\u{000c}'))
                .count();
            let property_chars = &chars[key_offset..];
            if property_chars.is_empty() || matches!(property_chars[0], '#' | '!') {
                return None;
            }

            let mut escaped = false;
            let key_end = property_chars
                .iter()
                .position(|character| {
                    if escaped {
                        escaped = false;
                        false
                    } else if *character == '\\' {
                        escaped = true;
                        false
                    } else {
                        matches!(character, '=' | ':' | ' ' | '\t' | '\u{000c}')
                    }
                })
                .unwrap_or(property_chars.len());

            let mut value_start = key_end;
            if value_start < property_chars.len()
                && matches!(property_chars[value_start], ' ' | '\t' | '\u{000c}')
            {
                while value_start < property_chars.len()
                    && matches!(property_chars[value_start], ' ' | '\t' | '\u{000c}')
                {
                    value_start += 1;
                }
                if value_start < property_chars.len()
                    && matches!(property_chars[value_start], '=' | ':')
                {
                    value_start += 1;
                }
            } else if value_start < property_chars.len() {
                value_start += 1;
            }
            while value_start < property_chars.len()
                && matches!(property_chars[value_start], ' ' | '\t' | '\u{000c}')
            {
                value_start += 1;
            }

            let key =
                unescape_java_properties(&property_chars[..key_end].iter().collect::<String>());
            let mut raw_key_end = key_end;
            if key_end < property_chars.len()
                && matches!(property_chars[key_end], ' ' | '\t' | '\u{000c}')
            {
                while raw_key_end < property_chars.len()
                    && matches!(property_chars[raw_key_end], ' ' | '\t' | '\u{000c}')
                {
                    raw_key_end += 1;
                }
                if raw_key_end >= property_chars.len() || property_chars[raw_key_end] != '=' {
                    raw_key_end = key_end;
                }
            }
            let raw_key = raw_key_complete
                .then(|| chars[..key_offset + raw_key_end].iter().collect::<String>());
            let value =
                unescape_java_properties(&property_chars[value_start..].iter().collect::<String>());
            Some((key, raw_key, value))
        })
        .collect()
}

/// Properties の有効な行に不正な Unicode エスケープがあるかを返す。
pub fn java_properties_has_invalid_unicode(text: &str) -> bool {
    java_properties_logical_lines(text).iter().any(|line| {
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c == '\\'
                && let Some('u') = chars.next()
            {
                let digits: String = chars.by_ref().take(4).collect();
                if digits.len() != 4 || !digits.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return true;
                }
            }
        }
        false
    })
}

fn java_properties_logical_lines(text: &str) -> Vec<String> {
    java_properties_logical_lines_with_key_completeness(text)
        .into_iter()
        .map(|(line, _)| line)
        .collect()
}

fn java_properties_logical_lines_with_key_completeness(text: &str) -> Vec<(String, bool)> {
    let mut lines = Vec::new();
    let mut logical_line = String::new();
    let mut continuing = false;
    let mut raw_key_complete = true;

    for physical_line in text.lines() {
        // Propertiesのコメント行は末尾のバックスラッシュで継続しない。
        // 継続処理より先に除外しないと、次の有効なlocale項目をコメントへ連結してしまう。
        if !continuing
            && matches!(
                physical_line
                    .trim_start_matches([' ', '\t', '\u{000c}'])
                    .chars()
                    .next(),
                Some('#' | '!')
            )
        {
            continue;
        }

        if continuing {
            logical_line.push_str(physical_line.trim_start_matches([' ', '\t', '\u{000c}']));
        } else {
            logical_line.clear();
            logical_line.push_str(physical_line);
            raw_key_complete = true;
        }

        let trailing_backslashes = logical_line
            .chars()
            .rev()
            .take_while(|character| *character == '\\')
            .count();
        continuing = trailing_backslashes % 2 == 1;
        if continuing {
            if !java_properties_has_key_separator(&logical_line) {
                raw_key_complete = false;
            }
            logical_line.pop();
        } else {
            lines.push((std::mem::take(&mut logical_line), raw_key_complete));
        }
    }

    if continuing {
        // Properties.loadでは末尾の継続用バックスラッシュは改行と同様に捨てられる。
        lines.push((logical_line, raw_key_complete));
    }
    lines
}

fn java_properties_has_key_separator(line: &str) -> bool {
    let mut key_started = false;
    let mut escaped = false;
    for character in line.chars() {
        if !key_started && matches!(character, ' ' | '\t' | '\u{000c}') {
            continue;
        }
        if escaped {
            escaped = false;
            key_started = true;
        } else if character == '\\' {
            escaped = true;
            key_started = true;
        } else if matches!(character, '=' | ':' | ' ' | '\t' | '\u{000c}') {
            return true;
        } else {
            key_started = true;
        }
    }
    false
}

fn unescape_java_properties(value: &str) -> String {
    let mut decoded = Vec::<u16>::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(character) = chars.next() {
        if character != '\\' {
            let mut units = [0; 2];
            decoded.extend_from_slice(character.encode_utf16(&mut units));
            continue;
        }

        let Some(escaped) = chars.next() else {
            decoded.push('\\' as u16);
            break;
        };
        match escaped {
            't' => decoded.push('\t' as u16),
            'n' => decoded.push('\n' as u16),
            'r' => decoded.push('\r' as u16),
            'f' => decoded.push('\u{000c}' as u16),
            'u' => {
                let mut digits = String::with_capacity(4);
                for _ in 0..4 {
                    if let Some(digit) = chars.next() {
                        digits.push(digit);
                    }
                }
                if digits.len() == 4 && digits.bytes().all(|digit| digit.is_ascii_hexdigit()) {
                    decoded.push(u16::from_str_radix(&digits, 16).expect("validated hex digits"));
                } else {
                    decoded.push('\\' as u16);
                    decoded.push('u' as u16);
                    decoded.extend(digits.encode_utf16());
                }
            }
            other => {
                let mut units = [0; 2];
                decoded.extend_from_slice(other.encode_utf16(&mut units));
            }
        }
    }

    String::from_utf16_lossy(&decoded)
}

/// Discovery結果のformatに従ってファイルを解析する。
pub fn parse_language_file(
    file: DiscoveredLanguageFile,
    bytes: &[u8],
) -> Result<ParsedLanguageFile, LanguageError> {
    let parsed = match file.format {
        LanguageFileFormat::Json => parse_json_language(bytes)?,
        LanguageFileFormat::LegacyLang => {
            parse_legacy_language_for_locale(bytes, Some(&file.locale))
        }
    };

    Ok(ParsedLanguageFile {
        file,
        entries: parsed.entries,
        warnings: parsed.warnings,
        legacy_parse_escapes: parsed.legacy_parse_escapes,
    })
}
