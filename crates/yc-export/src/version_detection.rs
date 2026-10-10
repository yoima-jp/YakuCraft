use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use yc_language::compare_minecraft_versions;

const RESOURCE_PACK_FORMATS: &str = include_str!("resource_pack_formats.json");

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MinecraftVersionDetection {
    pub version: String,
    pub method: MinecraftVersionDetectionMethod,
    pub original_value: Option<String>,
    pub rounded: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MinecraftVersionDetectionMethod {
    Metadata,
    PackMcmeta,
    ResourceStructure,
}

#[derive(Deserialize)]
struct FormatDatabase {
    formats: Vec<FormatRange>,
}

#[derive(Deserialize)]
struct FormatRange {
    from: String,
    to: String,
    pack_format: Value,
}

/// Modの宣言、pack.mcmeta、resource pathの順で互換性のある具体版を決める。
/// metadataのrangeは、Modが確実に対応する下限を優先する。
pub fn detect_minecraft_version(
    metadata_ranges: &[&str],
    pack_mcmeta: Option<&[u8]>,
    archive_paths: &[String],
) -> Option<MinecraftVersionDetection> {
    let database = format_database()?;

    for range in metadata_ranges {
        let Some((lower_bound, exclusive)) = metadata_lower_bound(range) else {
            continue;
        };
        let version = if exclusive {
            first_supported_version_after(&lower_bound, &database)
                .or_else(|| closest_supported_version(&lower_bound, &database))?
        } else {
            closest_supported_version(&lower_bound, &database)?
        };
        return Some(MinecraftVersionDetection {
            rounded: version != lower_bound,
            version,
            method: MinecraftVersionDetectionMethod::Metadata,
            original_value: Some((*range).to_owned()),
        });
    }

    if let Some(bytes) = pack_mcmeta
        && let Some((format, original_value)) = pack_metadata_format(bytes)
        && let Some((version, rounded)) = version_for_pack_format(format, &database)
    {
        return Some(MinecraftVersionDetection {
            version,
            method: MinecraftVersionDetectionMethod::PackMcmeta,
            original_value: Some(original_value),
            rounded,
        });
    }

    let (version, original_value) = infer_version_from_structure(archive_paths)?;
    let version = closest_supported_version(&version, &database)?;
    Some(MinecraftVersionDetection {
        version,
        method: MinecraftVersionDetectionMethod::ResourceStructure,
        original_value: Some(original_value.to_owned()),
        rounded: false,
    })
}

/// Selectに渡す、tableの範囲端点を一意なMinecraft順で返す。
pub fn supported_minecraft_versions() -> Vec<String> {
    let Some(database) = format_database() else {
        return Vec::new();
    };
    supported_versions_from(&database)
}

fn format_database() -> Option<FormatDatabase> {
    serde_json::from_str(RESOURCE_PACK_FORMATS).ok()
}

fn metadata_lower_bound(range: &str) -> Option<(String, bool)> {
    for alternative in range.split("||") {
        let first_bound = alternative
            .split_once(',')
            .map_or(alternative, |(lower, _)| lower)
            .trim();
        let Some(token) = first_bound.split_whitespace().next() else {
            continue;
        };
        if token.starts_with('<') || token == "*" {
            continue;
        }
        let (token, exclusive) = if let Some(token) = token.strip_prefix(">=") {
            (token, false)
        } else if let Some(token) = token.strip_prefix('>') {
            (token, true)
        } else {
            (
                token
                    .strip_prefix('~')
                    .or_else(|| token.strip_prefix('^'))
                    .or_else(|| token.strip_prefix('='))
                    .unwrap_or(token),
                false,
            )
        };
        let exclusive = exclusive || token.starts_with('(');
        let token = token
            .trim_start_matches('[')
            .trim_start_matches('(')
            .trim_start_matches('v');
        let candidate = token
            .strip_suffix(".x")
            .or_else(|| token.strip_suffix(".X"))
            .or_else(|| token.strip_suffix(".*"))
            .unwrap_or(token)
            .trim_end_matches([']', ')']);
        if parse_version(candidate).is_some() {
            return Some((candidate.to_owned(), exclusive));
        }
    }
    None
}

fn first_supported_version_after(version: &str, database: &FormatDatabase) -> Option<String> {
    supported_versions_from(database)
        .into_iter()
        .find(|candidate| {
            compare_minecraft_versions(candidate, version)
                .is_some_and(|order| order == Ordering::Greater)
        })
}

fn closest_supported_version(version: &str, database: &FormatDatabase) -> Option<String> {
    let target = parse_version(version)?;
    let versions = supported_versions_from(database);
    if let Some(exact) = versions.iter().find(|candidate| *candidate == version) {
        return Some(exact.clone());
    }

    let same_series: Vec<_> = versions
        .iter()
        .filter(|candidate| {
            parse_version(candidate)
                .is_some_and(|candidate| candidate[0] == target[0] && candidate[1] == target[1])
        })
        .collect();
    if let Some(nearest) = same_series.into_iter().min_by_key(|candidate| {
        parse_version(candidate)
            .map(|candidate| candidate[2].abs_diff(target[2]))
            .unwrap_or(u32::MAX)
    }) {
        return Some((*nearest).clone());
    }

    // 別のminor系列なら、まず表にある直前の版を使う。最古より前だけは直後を選ぶ。
    versions
        .iter()
        .rfind(|candidate| {
            compare_minecraft_versions(candidate, version)
                .is_some_and(|order| order != Ordering::Greater)
        })
        .or_else(|| versions.first())
        .cloned()
}

fn supported_versions_from(database: &FormatDatabase) -> Vec<String> {
    let mut versions: Vec<String> = database
        .formats
        .iter()
        .flat_map(|format| [format.from.clone(), format.to.clone()])
        .collect();
    versions.sort_by(|left, right| {
        compare_minecraft_versions(left, right)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.cmp(right))
    });
    versions.dedup();
    versions
}

fn pack_metadata_format(bytes: &[u8]) -> Option<((u32, u32), String)> {
    let root: Value = serde_json::from_slice(bytes).ok()?;
    let pack = root.get("pack")?;
    let pack_format = pack.get("pack_format").and_then(format_value);
    let min_format = pack.get("min_format").and_then(format_value);
    let max_format = pack.get("max_format").and_then(format_value);
    // 1.21.9以降のpackは旧clientとの互換のためpack_format（majorのみ）も併記するが、
    // 対象versionを表すのはmajor.minorまで持つmin_format。Modの対応下限を採る方針にも合うため、
    // min_formatを最優先し、旧形式のpackだけpack_formatを使う。
    let target = min_format.or(pack_format).or(max_format)?;
    let original_value = format!(
        "pack_format {}{}{}",
        pack.get("pack_format")
            .and_then(value_as_text)
            .unwrap_or_else(|| "不明".to_owned()),
        min_format
            .map(|format| format!(" (min_format {})", format_text(format)))
            .unwrap_or_default(),
        max_format
            .map(|format| format!(" (max_format {})", format_text(format)))
            .unwrap_or_default(),
    );
    Some((target, original_value))
}

fn version_for_pack_format(
    target: (u32, u32),
    database: &FormatDatabase,
) -> Option<(String, bool)> {
    let matches: Vec<_> = database
        .formats
        .iter()
        .filter_map(|format| Some((format, format_value(&format.pack_format)?)))
        .collect();
    if let Some(format) = matches.iter().find(|(_, value)| *value == target) {
        return Some((format.0.to.clone(), false));
    }
    let nearest = matches.into_iter().min_by_key(|(_, value)| {
        u64::from(value.0.abs_diff(target.0)) * 1_000_000 + u64::from(value.1.abs_diff(target.1))
    })?;
    Some((nearest.0.to.clone(), true))
}

fn format_value(value: &Value) -> Option<(u32, u32)> {
    match value {
        Value::Number(number) => Some((u32::try_from(number.as_u64()?).ok()?, 0)),
        Value::Array(parts) if parts.len() == 2 => Some((
            u32::try_from(parts[0].as_u64()?).ok()?,
            u32::try_from(parts[1].as_u64()?).ok()?,
        )),
        _ => None,
    }
}

fn value_as_text(value: &Value) -> Option<String> {
    match value {
        Value::Number(number) => Some(number.to_string()),
        Value::Array(_) => format_value(value).map(format_text),
        _ => None,
    }
}

fn format_text((major, minor): (u32, u32)) -> String {
    format!("[{major}, {minor}]")
}

fn infer_version_from_structure(paths: &[String]) -> Option<(String, &'static str)> {
    let mut has_legacy_lang = false;
    let mut has_json_lang = false;
    let mut has_item_definitions = false;
    for path in paths {
        let Some(rest) = path.strip_prefix("assets/") else {
            continue;
        };
        let parts: Vec<_> = rest.split('/').collect();
        if parts.len() < 3 || parts[0].is_empty() {
            continue;
        }
        match parts[1] {
            "items" if parts.len() >= 3 && path.to_ascii_lowercase().ends_with(".json") => {
                has_item_definitions = true;
            }
            "lang" if parts.len() >= 3 && path.to_ascii_lowercase().ends_with(".lang") => {
                has_legacy_lang = true;
            }
            "lang" if parts.len() >= 3 && path.to_ascii_lowercase().ends_with(".json") => {
                has_json_lang = true;
            }
            _ => {}
        }
    }
    if has_item_definitions {
        Some(("1.21.4".to_owned(), "assets/<namespace>/items/*.json"))
    } else if has_json_lang {
        Some(("1.13".to_owned(), "assets/<namespace>/lang/*.json"))
    } else if has_legacy_lang {
        Some(("1.12.2".to_owned(), "assets/<namespace>/lang/*.lang"))
    } else {
        None
    }
}

fn parse_version(version: &str) -> Option<[u32; 3]> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().map(str::parse).transpose().ok()?.unwrap_or(0);
    if parts.next().is_some() {
        return None;
    }
    Some([major, minor, patch])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(
        range: Option<&str>,
        pack_mcmeta: Option<&[u8]>,
        paths: &[&str],
    ) -> Option<MinecraftVersionDetection> {
        let metadata = range.into_iter().collect::<Vec<_>>();
        let paths = paths
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<Vec<_>>();
        detect_minecraft_version(&metadata, pack_mcmeta, &paths)
    }

    #[test]
    fn resolves_supported_metadata_range_forms() {
        for (range, expected) in [
            (">=1.20.1", "1.20.1"),
            ("~1.21", "1.21"),
            ("1.20.x", "1.20"),
            ("[1.20.1,1.21)", "1.20.1"),
            ("1.21.1", "1.21.1"),
            ("1.20.1 1.21.1", "1.20.1"),
            (">=1.20.1 <1.21", "1.20.1"),
        ] {
            let detection = detect(Some(range), None, &[]).unwrap();
            assert_eq!(detection.version, expected, "{range}");
            assert_eq!(detection.original_value.as_deref(), Some(range));
            assert_eq!(detection.method, MinecraftVersionDetectionMethod::Metadata);
        }
    }

    #[test]
    fn reverses_pack_mcmeta_formats() {
        let old_format = br#"{"pack":{"pack_format":15}}"#;
        let new_format = br#"{"pack":{"pack_format":69,"min_format":[69,0],"max_format":[69,0]}}"#;
        assert_eq!(
            detect(None, Some(old_format), &[]).unwrap().version,
            "1.20.1"
        );
        assert_eq!(
            detect(None, Some(new_format), &[]).unwrap().version,
            "1.21.10"
        );
        assert_eq!(
            detect(None, Some(new_format), &[]).unwrap().method,
            MinecraftVersionDetectionMethod::PackMcmeta
        );
    }

    #[test]
    fn prefers_min_format_minor_over_legacy_pack_format() {
        let both = br#"{"pack":{"pack_format":97,"min_format":[97,1],"max_format":[97,1]}}"#;
        let min_only = br#"{"pack":{"min_format":[97,1],"max_format":[97,1]}}"#;
        assert_eq!(
            detect(None, Some(both), &[]).unwrap().version,
            detect(None, Some(min_only), &[]).unwrap().version
        );
    }

    #[test]
    fn infers_version_from_language_and_item_asset_structure() {
        for (path, expected) in [
            ("assets/example/lang/en_us.lang", "1.12.2"),
            ("assets/example/lang/en_us.json", "1.13"),
            ("assets/example/items/example.json", "1.21.4"),
        ] {
            let detection = detect(None, None, &[path]).unwrap();
            assert_eq!(detection.version, expected, "{path}");
            assert_eq!(
                detection.method,
                MinecraftVersionDetectionMethod::ResourceStructure
            );
            assert!(!detection.rounded);
        }
    }

    #[test]
    fn rounds_to_nearest_same_series_then_previous_or_newest() {
        for (range, expected) in [
            ("1.20.9", "1.20.6"),
            ("1.22", "1.21.11"),
            ("27.0", "26.3"),
            ("1.5.2", "1.6.1"),
        ] {
            let detection = detect(Some(range), None, &[]).unwrap();
            assert_eq!(detection.version, expected, "{range}");
            assert!(detection.rounded, "{range}");
        }
    }

    #[test]
    fn metadata_takes_precedence_over_pack_metadata_and_structure() {
        let detection = detect(
            Some(">=1.20.1"),
            Some(br#"{"pack":{"pack_format":3}}"#),
            &["assets/example/lang/en_us.lang"],
        )
        .unwrap();
        assert_eq!(detection.version, "1.20.1");
        assert_eq!(detection.method, MinecraftVersionDetectionMethod::Metadata);
    }
}
