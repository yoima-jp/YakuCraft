mod icon;
mod json;
mod manifest;
#[path = "../../yc-language/src/parse_error.rs"]
mod parse_error;
mod toml_metadata;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use serde::Serialize;
use thiserror::Error;

pub use icon::{IconMediaType, ModIcon, find_mod_icon};
pub use json::parse_json_relaxed;

/// ZIP実装から独立して、アーカイブ内の名前と内容だけを受け取る。
pub trait ArchiveLookup {
    fn paths(&self) -> Vec<String>;
    fn read(&self, path: &str) -> Option<Vec<u8>>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataAnalysis {
    pub mods: Vec<ModInfo>,
    pub namespaces: Vec<String>,
    pub nested_jars: Vec<String>,
    pub warnings: Vec<MetadataWarning>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModInfo {
    pub mod_id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub loader: LoaderKind,
    /// 指定されたversion rangeの文字列をそのまま保持する。
    pub minecraft_version: Option<String>,
    /// rangeの下限などから読み取れた代表的なMinecraft version。
    pub minecraft_version_estimate: Option<String>,
    pub metadata_source: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoaderKind {
    Fabric,
    Quilt,
    Forge,
    #[serde(rename = "neoforge")]
    NeoForge,
    LegacyForge,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataWarning {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum MetadataParseError {
    #[error("JSON parse failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("TOML parse failed: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("metadata must contain a `{0}` array")]
    MissingArray(&'static str),
    #[error("metadata must contain a non-empty `{0}` string")]
    MissingField(&'static str),
}

/// アーカイブ内のMod metadataを、利用可能な形式ごとに独立して解析する。
pub fn analyze_metadata<A: ArchiveLookup + ?Sized>(archive: &A) -> MetadataAnalysis {
    const FORMAT_PATHS: &[(&str, LoaderKind)] = &[
        ("fabric.mod.json", LoaderKind::Fabric),
        ("quilt.mod.json", LoaderKind::Quilt),
        ("META-INF/neoforge.mods.toml", LoaderKind::NeoForge),
        ("META-INF/mods.toml", LoaderKind::Forge),
        ("mcmod.info", LoaderKind::LegacyForge),
    ];

    let paths: BTreeSet<String> = archive.paths().into_iter().collect();
    let mut result = MetadataAnalysis {
        namespaces: find_namespaces(&paths),
        ..MetadataAnalysis::default()
    };

    let manifest_path = "META-INF/MANIFEST.MF";
    let manifest = if paths.contains(manifest_path) {
        match archive.read(manifest_path) {
            Some(bytes) => {
                let (attributes, diagnostics) = manifest::parse_manifest(&bytes);
                for diagnostic in diagnostics {
                    add_warning(&mut result, manifest_path, diagnostic);
                }
                Some(attributes)
            }
            None => {
                add_warning(
                    &mut result,
                    manifest_path,
                    "アーカイブからファイルを読み取れませんでした".to_owned(),
                );
                None
            }
        }
    } else {
        None
    };

    for (path, loader) in FORMAT_PATHS {
        if !paths.contains(*path) {
            continue;
        }
        let Some(bytes) = archive.read(path) else {
            add_warning(
                &mut result,
                path,
                "アーカイブからファイルを読み取れませんでした".to_owned(),
            );
            continue;
        };

        let parsed = match loader {
            LoaderKind::Fabric => json::parse_fabric(&bytes, path),
            LoaderKind::Quilt => json::parse_quilt(&bytes, path),
            LoaderKind::Forge | LoaderKind::NeoForge => {
                toml_metadata::parse_mods(&bytes, path, *loader)
            }
            LoaderKind::LegacyForge => json::parse_legacy(&bytes, path),
            LoaderKind::Unknown => unreachable!("metadata paths always identify a loader"),
        };

        match parsed {
            Ok((mods, nested_jars)) => {
                result.mods.extend(mods);
                result.nested_jars.extend(nested_jars);
            }
            Err(error) => add_parse_warning(&mut result, path, &bytes, error),
        }
    }

    let jarjar_path = "META-INF/jarjar/metadata.json";
    if paths.contains(jarjar_path) {
        match archive.read(jarjar_path) {
            Some(bytes) => match json::parse_jarjar(&bytes) {
                Ok(nested_jars) => result.nested_jars.extend(nested_jars),
                Err(error) => add_parse_warning(&mut result, jarjar_path, &bytes, error),
            },
            None => add_warning(
                &mut result,
                jarjar_path,
                "アーカイブからファイルを読み取れませんでした".to_owned(),
            ),
        }
    }

    // metadataに記載された場所に加え、一般的な配置も走査し、重複は後で除く。
    for path in &paths {
        if is_direct_nested_jar(path, "META-INF/jars/")
            || is_direct_nested_jar(path, "META-INF/jarjar/")
        {
            result.nested_jars.push(path.clone());
        }
    }

    result.nested_jars.sort();
    result.nested_jars.dedup();

    if result.mods.is_empty() {
        result.mods = fallback_mods(&result.namespaces, manifest.as_ref());
    }

    let implementation_version = manifest
        .as_ref()
        .and_then(|attributes| attributes.implementation_version.as_deref());
    for mod_info in &mut result.mods {
        mod_info.version = complete_jar_version(mod_info.version.take(), implementation_version);
        mod_info.minecraft_version_estimate = mod_info
            .minecraft_version
            .as_deref()
            .and_then(estimate_minecraft_version);
    }

    result
}

pub(crate) fn make_mod_info(
    mod_id: Option<String>,
    name: Option<String>,
    version: Option<String>,
    loader: LoaderKind,
    minecraft_version: Option<String>,
    metadata_source: Option<String>,
) -> ModInfo {
    let minecraft_version_estimate = minecraft_version
        .as_deref()
        .and_then(estimate_minecraft_version);
    ModInfo {
        mod_id,
        name,
        version,
        loader,
        minecraft_version,
        minecraft_version_estimate,
        metadata_source,
    }
}

pub(crate) fn find_namespaces(paths: &BTreeSet<String>) -> Vec<String> {
    paths
        .iter()
        .filter_map(|path| {
            let rest = path.strip_prefix("assets/")?;
            let (namespace, _) = rest.split_once('/')?;
            (!namespace.is_empty()).then(|| namespace.to_owned())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn add_parse_warning(
    result: &mut MetadataAnalysis,
    path: &str,
    bytes: &[u8],
    error: MetadataParseError,
) {
    let detail = match &error {
        MetadataParseError::Json(source) => parse_error::parse_error_detail(
            &format!("JSON {:?}", source.classify()),
            path,
            source.line(),
            source.column(),
        ),
        MetadataParseError::Toml(source) => {
            let text = String::from_utf8_lossy(bytes);
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            let offset = source.span().map_or(0, |span| span.start).min(text.len());
            let prefix = text.get(..offset).unwrap_or("");
            let line = prefix.bytes().filter(|&byte| byte == b'\n').count() + 1;
            let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            parse_error::parse_error_detail("TOML", path, line, column)
        }
        _ => error.to_string(),
    };
    tracing::warn!(path, error = %detail, "Mod metadataを解析できませんでした");
    result.warnings.push(MetadataWarning {
        path: path.to_owned(),
        message: error.to_string(),
    });
}

pub(crate) fn add_warning(result: &mut MetadataAnalysis, path: &str, message: String) {
    tracing::warn!(path, error = %message, "Mod metadataを解析できませんでした");
    result.warnings.push(MetadataWarning {
        path: path.to_owned(),
        message,
    });
}

fn fallback_mods(
    namespaces: &[String],
    manifest: Option<&manifest::ManifestAttributes>,
) -> Vec<ModInfo> {
    let Some(manifest) = manifest else {
        if namespaces.is_empty() {
            return vec![make_mod_info(
                None,
                None,
                None,
                LoaderKind::Unknown,
                None,
                None,
            )];
        }
        return namespaces
            .iter()
            .map(|namespace| {
                make_mod_info(
                    Some(namespace.clone()),
                    Some(namespace.clone()),
                    None,
                    LoaderKind::Unknown,
                    None,
                    Some(format!("assets/{namespace}/")),
                )
            })
            .collect();
    };

    let source = Some("META-INF/MANIFEST.MF".to_owned());
    if namespaces.is_empty() {
        return vec![make_mod_info(
            manifest.automatic_module_name.clone(),
            manifest.implementation_title.clone(),
            manifest.implementation_version.clone(),
            LoaderKind::Unknown,
            None,
            source,
        )];
    }

    namespaces
        .iter()
        .map(|namespace| {
            make_mod_info(
                Some(namespace.clone()),
                manifest
                    .implementation_title
                    .clone()
                    .or_else(|| Some(namespace.clone())),
                manifest.implementation_version.clone(),
                LoaderKind::Unknown,
                None,
                source.clone(),
            )
        })
        .collect()
}

fn is_direct_nested_jar(path: &str, prefix: &str) -> bool {
    let Some(file_name) = path.strip_prefix(prefix) else {
        return false;
    };
    !file_name.is_empty()
        && !file_name.contains('/')
        && file_name.to_ascii_lowercase().ends_with(".jar")
}

fn complete_jar_version(
    version: Option<String>,
    implementation_version: Option<&str>,
) -> Option<String> {
    const PLACEHOLDER: &str = "${file.jarVersion}";
    version.map(
        |value| match implementation_version.filter(|v| !v.is_empty()) {
            Some(replacement) if value.contains(PLACEHOLDER) => {
                value.replace(PLACEHOLDER, replacement)
            }
            _ => value,
        },
    )
}

fn estimate_minecraft_version(range: &str) -> Option<String> {
    let range = range.trim();
    let candidate = if let Some(bounds) = range.strip_prefix('[') {
        let end = bounds.find([',', ']', ')']).unwrap_or(bounds.len());
        bounds[..end].trim()
    } else if let Some(lower_bound) = range.strip_prefix(">=") {
        lower_bound.trim()
    } else if let Some(version) = range.strip_prefix('~') {
        version.trim()
    } else if let Some(version) = range.strip_prefix('^') {
        version.trim()
    } else if let Some(version) = range.strip_prefix('=') {
        version.trim()
    } else if range.starts_with(['<', '>', '(']) || range.is_empty() {
        return None;
    } else {
        range
    };

    let end = candidate
        .find(|character: char| !(character.is_ascii_digit() || character == '.'))
        .unwrap_or(candidate.len());
    let version = &candidate[..end];
    if version.is_empty()
        || !version.contains('.')
        || version.starts_with('.')
        || version.ends_with('.')
        || version.split('.').any(|part| part.is_empty())
    {
        return None;
    }
    Some(version.to_owned())
}
