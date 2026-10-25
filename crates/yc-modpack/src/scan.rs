use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use crate::{
    ModpackError,
    ftb::{add_ftb_entries, is_translatable, parse_ftb_document},
    kubejs::{add_kubejs_entries, parse_kubejs_document},
    model::{
        FtbDocument, FtbQuestsStatus, KubeJsOutputDocument, ModpackData, ModpackFile,
        ModpackFileKind, ModpackScan,
    },
};

const FTB_LANG_PREFIX: &str = "config/ftbquests/quests/lang/";
const FTB_CHAPTERS_PREFIX: &str = "config/ftbquests/quests/chapters/";

#[derive(Clone, Debug)]
struct FtbLanguagePath {
    path: String,
    locale: String,
    split_relative: Option<String>,
}

#[derive(Clone, Debug)]
struct KubeJsLanguagePath {
    path: String,
    namespace: String,
    locale: String,
}

/// Analyze an imported set of instance-relative files without reading from disk.
pub fn analyze_files(files: &BTreeMap<String, Vec<u8>>) -> Result<ModpackData, ModpackError> {
    let normalized = normalize_file_map(files)?;
    let game_prefix = find_game_prefix(normalized.keys().map(String::as_str));
    let game_files = game_files(&normalized, &game_prefix);
    let ftb_paths = find_ftb_language_paths(game_files.keys().map(String::as_str));
    let kubejs_paths = find_kubejs_language_paths(game_files.keys().map(String::as_str));
    let has_legacy_chapters = game_files.keys().any(|path| is_legacy_chapter(path));
    let ftb_status = ftb_status(&ftb_paths, has_legacy_chapters);

    let ftb_sources = english_ftb_sources(&ftb_paths);
    let kubejs_sources: Vec<_> = kubejs_paths
        .iter()
        .filter(|path| path.locale.eq_ignore_ascii_case("en_us"))
        .cloned()
        .collect();

    let mut ftb_documents = Vec::with_capacity(ftb_sources.len());
    let mut kubejs_documents = Vec::with_capacity(kubejs_sources.len());
    let mut entries = Vec::new();

    for source_path in ftb_sources {
        let target_game_path = counterpart_ftb_path(&source_path, "ja_jp");
        let source_instance_path = add_game_prefix(&game_prefix, &source_path.path);
        let target_instance_path = add_game_prefix(&game_prefix, &target_game_path);
        let source_bytes = game_files
            .get(&source_path.path)
            .expect("discovered FTB source file exists");
        let source = parse_ftb_document(&source_instance_path, source_bytes)?;
        let existing_translation = game_files
            .get(&target_game_path)
            .map(|bytes| {
                parse_ftb_document(&add_game_prefix(&game_prefix, &target_game_path), bytes)
            })
            .transpose()?;
        let key_path = source_path
            .split_relative
            .clone()
            .unwrap_or_else(|| "en_us.snbt".to_owned());
        let document = FtbDocument {
            source_path: source_instance_path,
            output_path: target_instance_path,
            key_path,
            source,
            existing_translation,
        };
        add_ftb_entries(&mut entries, &document);
        ftb_documents.push(document);
    }

    for source_path in kubejs_sources {
        let target_game_path = kubejs_counterpart_path(&source_path, "ja_jp");
        let source_instance_path = add_game_prefix(&game_prefix, &source_path.path);
        let target_instance_path = add_game_prefix(&game_prefix, &target_game_path);
        let source_bytes = game_files
            .get(&source_path.path)
            .expect("discovered KubeJS source file exists");
        let source = parse_kubejs_document(&source_instance_path, source_bytes)?;
        let existing_translation = game_files
            .get(&target_game_path)
            .map(|bytes| {
                parse_kubejs_document(&add_game_prefix(&game_prefix, &target_game_path), bytes)
            })
            .transpose()?;
        let document = KubeJsOutputDocument {
            source_path: source_instance_path,
            output_path: target_instance_path,
            namespace: source_path.namespace,
            source,
            existing_translation,
        };
        add_kubejs_entries(&mut entries, &document);
        kubejs_documents.push(document);
    }

    let ftb_quest_entries = entries
        .iter()
        .filter(|entry| entry.key.starts_with("ftbquests:"))
        .count();
    let kubejs_entries = entries.len() - ftb_quest_entries;
    let mut scan = build_scan(
        &game_prefix,
        &ftb_paths,
        &kubejs_paths,
        ftb_status,
        entries.len(),
        ftb_quest_entries,
        kubejs_entries,
    );
    if scan.ftb_quests == FtbQuestsStatus::LegacyInline {
        scan.total_entries = entries.len();
    }
    Ok(ModpackData::new(
        scan,
        entries,
        ftb_documents,
        kubejs_documents,
    ))
}

/// Resolve a launcher instance root to its game directory when it contains a
/// `minecraft/` or `.minecraft/` child. A directory that already looks like a
/// game directory is returned unchanged.
pub fn resolve_game_directory(root: &Path) -> PathBuf {
    if looks_like_game_directory(root) {
        return root.to_path_buf();
    }
    for child in ["minecraft", ".minecraft"] {
        let candidate = root.join(child);
        if looks_like_game_directory(&candidate) {
            return candidate;
        }
    }
    root.to_path_buf()
}

/// Scan an installed instance. Individual unreadable or malformed translation
/// files are reported in `issues`; other discovered files remain available.
pub fn scan_instance(root: &Path) -> ModpackScan {
    let game_directory = resolve_game_directory(root);
    let game_prefix = game_directory
        .strip_prefix(root)
        .ok()
        .map(path_to_slashes)
        .unwrap_or_default();
    let game_prefix = game_prefix.trim_matches('/').to_owned();
    let mut issues = Vec::new();
    let mut files = BTreeMap::new();

    let ftb_lang_directory = game_directory.join("config/ftbquests/quests/lang");
    if ftb_lang_directory.is_dir() {
        collect_matching_files(
            &ftb_lang_directory,
            &game_directory,
            "config/ftbquests/quests/lang",
            "snbt",
            true,
            &mut files,
            &mut issues,
        );
    }

    collect_kubejs_language_files(&game_directory, &mut files, &mut issues);

    let mut instance_files = BTreeMap::new();
    for (path, bytes) in files {
        instance_files.insert(add_game_prefix(&game_prefix, &path), bytes);
    }

    let has_legacy_chapters =
        has_snbt_files(&game_directory.join("config/ftbquests/quests/chapters"));
    let normalized = match normalize_file_map(&instance_files) {
        Ok(files) => files,
        Err(error) => {
            issues.push(error.to_string());
            BTreeMap::new()
        }
    };
    let prefix = find_game_prefix(normalized.keys().map(String::as_str));
    let game_files = game_files(&normalized, &prefix);
    let ftb_paths = find_ftb_language_paths(game_files.keys().map(String::as_str));
    let kubejs_paths = find_kubejs_language_paths(game_files.keys().map(String::as_str));
    let status = ftb_status(&ftb_paths, has_legacy_chapters);
    let (ftb_quest_entries, kubejs_entries) =
        count_source_entries(&game_files, &prefix, &ftb_paths, &kubejs_paths, &mut issues);
    let total_entries = ftb_quest_entries + kubejs_entries;
    let mut scan = build_scan(
        &prefix,
        &ftb_paths,
        &kubejs_paths,
        status,
        total_entries,
        ftb_quest_entries,
        kubejs_entries,
    );
    scan.game_directory = if game_prefix.is_empty() {
        ".".to_owned()
    } else {
        game_prefix
    };
    scan.issues = issues;
    scan
}

fn count_source_entries(
    game_files: &BTreeMap<String, &Vec<u8>>,
    game_prefix: &str,
    ftb_paths: &[FtbLanguagePath],
    kubejs_paths: &[KubeJsLanguagePath],
    issues: &mut Vec<String>,
) -> (usize, usize) {
    let mut ftb_count = 0;
    for source in english_ftb_sources(ftb_paths) {
        let path = add_game_prefix(game_prefix, &source.path);
        let Some(bytes) = game_files.get(&source.path) else {
            continue;
        };
        match parse_ftb_document(&path, bytes) {
            Ok(document) => {
                ftb_count += document
                    .entries
                    .iter()
                    .map(|(_, value)| match value {
                        crate::SnbtValue::String(value) => usize::from(is_translatable(value)),
                        crate::SnbtValue::StringArray(values) => {
                            values.iter().filter(|value| is_translatable(value)).count()
                        }
                    })
                    .sum::<usize>();
            }
            Err(error) => issues.push(error.to_string()),
        }
    }
    let mut kubejs_count = 0;
    for source in kubejs_paths
        .iter()
        .filter(|path| path.locale.eq_ignore_ascii_case("en_us"))
    {
        let path = add_game_prefix(game_prefix, &source.path);
        let Some(bytes) = game_files.get(&source.path) else {
            continue;
        };
        match parse_kubejs_document(&path, bytes) {
            Ok(document) => {
                kubejs_count += document
                    .entries
                    .values()
                    .filter(|value| !value.trim().is_empty())
                    .count()
            }
            Err(error) => issues.push(error.to_string()),
        }
    }
    (ftb_count, kubejs_count)
}

fn normalize_file_map(
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, Vec<u8>>, ModpackError> {
    files
        .iter()
        .map(|(path, bytes)| Ok((normalize_instance_path(path)?, bytes.clone())))
        .collect()
}

fn normalize_instance_path(path: &str) -> Result<String, ModpackError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.as_bytes().get(1) == Some(&b':')
    {
        return Err(ModpackError::InvalidPath(path.to_owned()));
    }
    let parsed = Path::new(path);
    if parsed.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(ModpackError::InvalidPath(path.to_owned()));
    }
    let normalized = path
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<_>>()
        .join("/");
    if normalized.is_empty() {
        return Err(ModpackError::InvalidPath(path.to_owned()));
    }
    Ok(normalized)
}

fn find_game_prefix<'a>(paths: impl Iterator<Item = &'a str>) -> String {
    let paths: Vec<_> = paths.collect();
    if paths
        .iter()
        .any(|path| path.starts_with("config/") || path.starts_with("kubejs/"))
    {
        return String::new();
    }
    for prefix in ["minecraft/", ".minecraft/"] {
        if paths.iter().any(|path| {
            path.strip_prefix(prefix)
                .is_some_and(|path| path.starts_with("config/") || path.starts_with("kubejs/"))
        }) {
            return prefix.trim_end_matches('/').to_owned();
        }
    }
    String::new()
}

fn game_files<'a>(
    files: &'a BTreeMap<String, Vec<u8>>,
    prefix: &str,
) -> BTreeMap<String, &'a Vec<u8>> {
    let prefix = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    files
        .iter()
        .filter_map(|(path, bytes)| {
            if prefix.is_empty() {
                (!path.starts_with("minecraft/") && !path.starts_with(".minecraft/"))
                    .then(|| (path.clone(), bytes))
            } else {
                path.strip_prefix(&prefix)
                    .map(|relative| (relative.to_owned(), bytes))
            }
        })
        .collect()
}

fn find_ftb_language_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<FtbLanguagePath> {
    paths
        .filter_map(|path| {
            let relative = path.strip_prefix(FTB_LANG_PREFIX)?;
            let components: Vec<_> = relative.split('/').collect();
            match components.as_slice() {
                [file] if extension_is(file, "snbt") => Some(FtbLanguagePath {
                    path: path.to_owned(),
                    locale: file[..file.len() - ".snbt".len()].to_owned(),
                    split_relative: None,
                }),
                [locale, nested @ ..]
                    if !nested.is_empty() && extension_is(nested.last()?, "snbt") =>
                {
                    Some(FtbLanguagePath {
                        path: path.to_owned(),
                        locale: (*locale).to_owned(),
                        split_relative: Some(nested.join("/")),
                    })
                }
                _ => None,
            }
        })
        .collect()
}

fn find_kubejs_language_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<KubeJsLanguagePath> {
    paths
        .filter_map(|path| {
            let components: Vec<_> = path.split('/').collect();
            match components.as_slice() {
                ["kubejs", "assets", namespace, "lang", file] if extension_is(file, "json") => {
                    Some(KubeJsLanguagePath {
                        path: path.to_owned(),
                        namespace: (*namespace).to_owned(),
                        locale: file[..file.len() - ".json".len()].to_owned(),
                    })
                }
                _ => None,
            }
        })
        .collect()
}

fn ftb_status(paths: &[FtbLanguagePath], has_legacy_chapters: bool) -> FtbQuestsStatus {
    if paths
        .iter()
        .any(|path| path.locale.eq_ignore_ascii_case("en_us") && path.split_relative.is_some())
    {
        FtbQuestsStatus::Split
    } else if paths
        .iter()
        .any(|path| path.locale.eq_ignore_ascii_case("en_us") && path.split_relative.is_none())
    {
        FtbQuestsStatus::Single
    } else if paths.iter().any(|path| path.split_relative.is_some()) {
        FtbQuestsStatus::Split
    } else if !paths.is_empty() {
        FtbQuestsStatus::Single
    } else if has_legacy_chapters {
        FtbQuestsStatus::LegacyInline
    } else {
        FtbQuestsStatus::None
    }
}

fn english_ftb_sources(paths: &[FtbLanguagePath]) -> Vec<FtbLanguagePath> {
    let split_sources: Vec<_> = paths
        .iter()
        .filter(|path| path.locale.eq_ignore_ascii_case("en_us") && path.split_relative.is_some())
        .cloned()
        .collect();
    if split_sources.is_empty() {
        paths
            .iter()
            .filter(|path| {
                path.locale.eq_ignore_ascii_case("en_us") && path.split_relative.is_none()
            })
            .cloned()
            .collect()
    } else {
        split_sources
    }
}

fn counterpart_ftb_path(source: &FtbLanguagePath, target_locale: &str) -> String {
    match &source.split_relative {
        Some(relative) => format!("{FTB_LANG_PREFIX}{target_locale}/{relative}"),
        None => format!("{FTB_LANG_PREFIX}{target_locale}.snbt"),
    }
}

fn kubejs_counterpart_path(source: &KubeJsLanguagePath, target_locale: &str) -> String {
    format!(
        "kubejs/assets/{}/lang/{target_locale}.json",
        source.namespace
    )
}

fn build_scan(
    game_prefix: &str,
    ftb_paths: &[FtbLanguagePath],
    kubejs_paths: &[KubeJsLanguagePath],
    ftb_quests: FtbQuestsStatus,
    total_entries: usize,
    ftb_quest_entries: usize,
    kubejs_entries: usize,
) -> ModpackScan {
    let mut files = Vec::with_capacity(ftb_paths.len() + kubejs_paths.len());
    files.extend(ftb_paths.iter().map(|file| ModpackFile {
        path: add_game_prefix(game_prefix, &file.path),
        kind: ModpackFileKind::FtbQuestsLanguage,
        locale: file.locale.clone(),
        namespace: None,
    }));
    files.extend(kubejs_paths.iter().map(|file| ModpackFile {
        path: add_game_prefix(game_prefix, &file.path),
        kind: ModpackFileKind::KubeJsLanguage,
        locale: file.locale.clone(),
        namespace: Some(file.namespace.clone()),
    }));
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let kubejs_namespaces = kubejs_paths
        .iter()
        .map(|file| file.namespace.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    ModpackScan {
        game_directory: if game_prefix.is_empty() {
            ".".to_owned()
        } else {
            game_prefix.to_owned()
        },
        ftb_quests,
        files,
        kubejs_namespaces,
        total_entries,
        ftb_quest_entries,
        kubejs_entries,
        issues: Vec::new(),
    }
}

fn add_game_prefix(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        path.to_owned()
    } else {
        format!("{prefix}/{path}")
    }
}

fn is_legacy_chapter(path: &str) -> bool {
    path.strip_prefix(FTB_CHAPTERS_PREFIX)
        .is_some_and(|relative| extension_is(relative, "snbt"))
}

fn extension_is(path: &str, expected: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case(expected))
}

fn looks_like_game_directory(path: &Path) -> bool {
    path.join("config").is_dir() || path.join("kubejs").is_dir()
}

fn collect_matching_files(
    directory: &Path,
    game_directory: &Path,
    relative_directory: &str,
    extension: &str,
    recursive: bool,
    files: &mut BTreeMap<String, Vec<u8>>,
    issues: &mut Vec<String>,
) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            issues.push(format!("{}: {error}", directory.display()));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                issues.push(format!("{}: {error}", directory.display()));
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                issues.push(format!("{}: {error}", entry.path().display()));
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if file_type.is_dir() && recursive {
            collect_matching_files(
                &entry.path(),
                game_directory,
                &format!("{relative_directory}/{name}"),
                extension,
                recursive,
                files,
                issues,
            );
        } else if file_type.is_file() && extension_is(&name, extension) {
            let relative_path = format!("{relative_directory}/{name}");
            let full_path = game_directory.join(&relative_path);
            match fs::read(&full_path) {
                Ok(bytes) => {
                    files.insert(relative_path, bytes);
                }
                Err(error) => issues.push(format!("{}: {error}", full_path.display())),
            }
        }
    }
}

fn collect_kubejs_language_files(
    game_directory: &Path,
    files: &mut BTreeMap<String, Vec<u8>>,
    issues: &mut Vec<String>,
) {
    let assets = game_directory.join("kubejs/assets");
    let namespaces = match fs::read_dir(&assets) {
        Ok(namespaces) => namespaces,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            issues.push(format!("{}: {error}", assets.display()));
            return;
        }
    };
    for namespace in namespaces {
        let namespace = match namespace {
            Ok(namespace) => namespace,
            Err(error) => {
                issues.push(format!("{}: {error}", assets.display()));
                continue;
            }
        };
        if !namespace
            .file_type()
            .is_ok_and(|file_type| file_type.is_dir())
        {
            continue;
        }
        let namespace_name = namespace.file_name().to_string_lossy().into_owned();
        let lang_dir = namespace.path().join("lang");
        let relative = format!("kubejs/assets/{namespace_name}/lang");
        if lang_dir.is_dir() {
            collect_matching_files(
                &lang_dir,
                game_directory,
                &relative,
                "json",
                false,
                files,
                issues,
            );
        }
    }
}

fn has_snbt_files(directory: &Path) -> bool {
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let Ok(file_type) = entry.file_type() else {
            return false;
        };
        if file_type.is_file() {
            extension_is(&entry.file_name().to_string_lossy(), "snbt")
        } else if file_type.is_dir() {
            has_snbt_files(&entry.path())
        } else {
            false
        }
    })
}

fn path_to_slashes(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
