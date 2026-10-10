use serde::{Deserialize, Serialize};

use crate::{DiscoveredLanguageFile, LanguageFileFormat};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageArchivePath {
    pub archive_path: String,
    /// Root JARは空文字、Nested JARはRoot JAR内のarchive path。
    pub container_path: String,
}

impl LanguageArchivePath {
    pub fn root(archive_path: impl Into<String>) -> Self {
        Self {
            archive_path: archive_path.into(),
            container_path: String::new(),
        }
    }

    pub fn nested(container_path: impl Into<String>, archive_path: impl Into<String>) -> Self {
        Self {
            archive_path: archive_path.into(),
            container_path: container_path.into(),
        }
    }
}

/// Archive path一覧からMinecraftの言語ファイルだけを抽出する。
pub fn discover_language_files(paths: &[LanguageArchivePath]) -> Vec<DiscoveredLanguageFile> {
    let mut files = paths
        .iter()
        .filter_map(|path| {
            let archive_path = path.archive_path.replace('\\', "/");
            let components: Vec<_> = archive_path.split('/').collect();
            if components.len() != 4 || components[0] != "assets" || components[2] != "lang" {
                return None;
            }

            let namespace = components[1].to_owned();
            // namespaceはResource Pack内のdirectory名にも使うため、親directoryを指す値は受け付けない。
            if namespace.is_empty() || namespace == "." || namespace == ".." {
                return None;
            }

            let file_name = components[3].to_owned();
            let (locale, extension) = file_name.rsplit_once('.')?;
            if locale.is_empty() {
                return None;
            }
            let locale = locale.to_owned();

            let format = if extension.eq_ignore_ascii_case("json") {
                LanguageFileFormat::Json
            } else if extension.eq_ignore_ascii_case("lang") {
                LanguageFileFormat::LegacyLang
            } else {
                return None;
            };

            Some(DiscoveredLanguageFile {
                namespace,
                normalized_locale: normalize_locale(&locale),
                locale,
                format,
                container_path: path.container_path.replace('\\', "/"),
                archive_path,
                file_name,
            })
        })
        .collect::<Vec<_>>();

    files.sort_by(|left, right| file_priority(left).cmp(&file_priority(right)));
    files
}

/// Minecraft localeは表記を保持し、比較時だけASCII小文字へ正規化する。
pub fn normalize_locale(locale: &str) -> String {
    locale.to_ascii_lowercase()
}

pub(crate) fn file_priority(file: &DiscoveredLanguageFile) -> (bool, &str, &str, &str, &str) {
    (
        !file.container_path.is_empty(),
        &file.container_path,
        &file.namespace,
        &file.archive_path,
        &file.locale,
    )
}
