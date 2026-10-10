use std::{
    collections::HashMap,
    env,
    fs::{self, File},
    io::{self, BufWriter, Read, Seek, Write},
    path::{Path, PathBuf},
};

use serde::Serialize;
use yc_archive::{Archive, EntryKind};
use yc_language::{
    LanguageArchivePath, ParsedLanguageFile, discover_language_files, parse_language_file,
};
use yc_validation::{PlaceholderDifference, PlaceholderSeverity, compare_placeholders};

#[derive(Serialize)]
struct WarningRecord<'a> {
    mod_directory: &'a str,
    jar_path: String,
    container_path: String,
    namespace: String,
    key: String,
    source: String,
    translation: String,
    severity: Option<PlaceholderSeverity>,
    difference: PlaceholderDifference,
}

#[derive(Default)]
struct ScanSummary {
    jar_count: usize,
    paired_languages: usize,
    key_pairs: usize,
    accepted_positional_rewrites: usize,
    warnings: usize,
    failures: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    let (output_path, roots) = arguments
        .split_last()
        .ok_or("usage: placeholder_corpus <mods-dir>... <output.jsonl>")?;
    if roots.is_empty() {
        return Err("at least one mods directory is required".into());
    }

    let mut output = BufWriter::new(File::create(PathBuf::from(output_path))?);
    let mut summaries = Vec::new();
    for root in roots {
        let root = PathBuf::from(root);
        let mod_directory = root
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("mods");
        let mut summary = ScanSummary::default();
        let mut jars = Vec::new();
        collect_jars(&root, &mut jars)?;
        summary.jar_count = jars.len();
        for jar_path in jars {
            let mut archive = match Archive::open(&jar_path) {
                Ok(archive) => archive,
                Err(error) => {
                    eprintln!("archive error: {}: {error}", jar_path.display());
                    summary.failures += 1;
                    continue;
                }
            };

            scan_archive(
                &mut archive,
                mod_directory,
                &jar_path.display().to_string(),
                "",
                &mut output,
                &mut summary,
            )?;

            match archive.discover_nested(&[]) {
                Ok(nested_archives) => {
                    for nested in nested_archives {
                        if let Some(mut nested_archive) = nested.archive {
                            scan_archive(
                                &mut nested_archive,
                                mod_directory,
                                &jar_path.display().to_string(),
                                &nested.path,
                                &mut output,
                                &mut summary,
                            )?;
                        }
                    }
                }
                Err(error) => {
                    eprintln!("nested archive error: {}: {error}", jar_path.display());
                    summary.failures += 1;
                }
            }
        }
        eprintln!(
            "{}: jars={}, paired_files={}, key_pairs={}, positional_rewrites={}, warnings={}, failures={}",
            root.display(),
            summary.jar_count,
            summary.paired_languages,
            summary.key_pairs,
            summary.accepted_positional_rewrites,
            summary.warnings,
            summary.failures,
        );
        summaries.push(summary);
    }
    output.flush()?;

    let total = summaries
        .iter()
        .fold(ScanSummary::default(), |mut total, item| {
            total.jar_count += item.jar_count;
            total.paired_languages += item.paired_languages;
            total.key_pairs += item.key_pairs;
            total.accepted_positional_rewrites += item.accepted_positional_rewrites;
            total.warnings += item.warnings;
            total.failures += item.failures;
            total
        });
    eprintln!(
        "total: jars={}, paired_files={}, key_pairs={}, positional_rewrites={}, warnings={}, failures={}, output={}",
        total.jar_count,
        total.paired_languages,
        total.key_pairs,
        total.accepted_positional_rewrites,
        total.warnings,
        total.failures,
        Path::new(output_path).display(),
    );
    Ok(())
}

fn collect_jars(directory: &Path, jars: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_jars(&path, jars)?;
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
        {
            jars.push(path);
        }
    }
    jars.sort();
    Ok(())
}

fn scan_archive<R: Read + Seek>(
    archive: &mut Archive<R>,
    mod_directory: &str,
    jar_path: &str,
    container_path: &str,
    output: &mut impl Write,
    summary: &mut ScanSummary,
) -> Result<(), Box<dyn std::error::Error>> {
    let paths: Vec<_> = archive
        .list_entries()
        .iter()
        .filter_map(|entry| entry.normalized_path.as_deref())
        .map(|archive_path| {
            if container_path.is_empty() {
                LanguageArchivePath::root(archive_path)
            } else {
                LanguageArchivePath::nested(container_path, archive_path)
            }
        })
        .collect();
    let languages: Vec<_> = discover_language_files(&paths)
        .into_iter()
        .filter(|file| matches!(file.normalized_locale.as_str(), "en_us" | "ja_jp"))
        .collect();

    let mut files = HashMap::<(String, String), HashMap<String, ParsedLanguageFile>>::new();
    for file in languages {
        let bytes = match archive.read_entry(&file.archive_path, EntryKind::Text) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!(
                    "language read error: {jar_path}!{}: {error}",
                    file.archive_path
                );
                summary.failures += 1;
                continue;
            }
        };
        match parse_language_file(file.clone(), &bytes) {
            Ok(parsed) => {
                files
                    .entry((file.container_path.clone(), file.namespace.clone()))
                    .or_default()
                    .insert(file.normalized_locale.clone(), parsed);
            }
            Err(error) => {
                eprintln!(
                    "language parse error: {jar_path}!{}: {error}",
                    file.archive_path
                );
                summary.failures += 1;
            }
        }
    }

    for ((container, namespace), locales) in files {
        let (Some(source), Some(translation)) = (locales.get("en_us"), locales.get("ja_jp")) else {
            continue;
        };
        summary.paired_languages += 1;
        compare_language_pair(
            source,
            translation,
            mod_directory,
            jar_path,
            &container,
            &namespace,
            output,
            summary,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn compare_language_pair(
    source: &ParsedLanguageFile,
    translation: &ParsedLanguageFile,
    mod_directory: &str,
    jar_path: &str,
    container_path: &str,
    namespace: &str,
    output: &mut impl Write,
    summary: &mut ScanSummary,
) -> Result<(), Box<dyn std::error::Error>> {
    for (key, source_value) in &source.entries {
        let Some(source_text) = source_value.text.as_deref() else {
            continue;
        };
        let Some(translation_text) = translation
            .entries
            .get(key)
            .and_then(|value| value.text.as_deref())
        else {
            continue;
        };
        summary.key_pairs += 1;
        let comparison = compare_placeholders(source_text, translation_text);
        if comparison.is_valid && is_positional_rewrite(source_text, translation_text) {
            summary.accepted_positional_rewrites += 1;
            if summary.accepted_positional_rewrites <= 3 {
                eprintln!(
                    "accepted positional rewrite: {jar_path}!{container_path} {namespace}:{key}: {source_text:?} => {translation_text:?}"
                );
            }
        }
        if comparison.is_valid {
            continue;
        }
        summary.warnings += 1;
        serde_json::to_writer(
            &mut *output,
            &WarningRecord {
                mod_directory,
                jar_path: jar_path.to_owned(),
                container_path: container_path.to_owned(),
                namespace: namespace.to_owned(),
                key: key.clone(),
                source: source_text.to_owned(),
                translation: translation_text.to_owned(),
                severity: comparison.severity,
                difference: comparison.difference,
            },
        )?;
        output.write_all(b"\n")?;
    }
    Ok(())
}

fn is_positional_rewrite(source: &str, translation: &str) -> bool {
    let source_tokens = yc_validation::extract_placeholders(source);
    let target_tokens = yc_validation::extract_placeholders(translation);
    let is_argument = |token: &&yc_validation::PlaceholderToken| {
        token.kind == yc_validation::PlaceholderKind::Printf
            && token.value != "%n"
            && token.value != "%%"
    };
    source_tokens
        .iter()
        .filter(is_argument)
        .any(|token| !token.value.contains('$'))
        && target_tokens
            .iter()
            .filter(is_argument)
            .any(|token| token.value.contains('$'))
}
