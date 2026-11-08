//! シェーダーパックを検出し、言語ファイルと ZIP を読み書きする。
//! `analyze_source` で ZIP またはフォルダを解析し、`parse_lang` で言語を読み込む。
//! フォルダは `pack_folder` で ZIP に変換し、`write_translated_zip` で日本語化する。

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[derive(Clone, Debug)]
pub enum ShaderPackSource {
    Zip(PathBuf),
    Folder(PathBuf),
}
#[derive(Clone, Debug)]
pub struct LangFile {
    pub path: String,
    pub filename: String,
    pub locale: String,
}
#[derive(Clone, Debug)]
pub struct Analysis {
    pub root: String,
    pub files: Vec<LangFile>,
    pub display_name: String,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct ParsedLang {
    pub entries: Vec<(String, String)>,
    /// Propertiesで解釈したキーから、元ファイルに書かれたキー表記への対応。
    pub raw_keys: HashMap<String, String>,
    pub warnings: Vec<LangParseWarning>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LangParseWarning {
    InvalidUtf8,
    InvalidUnicodeEscape,
    DuplicateKey { key: String },
}

fn safe(name: &str) -> bool {
    !name.starts_with(['/', '\\'])
        && !name.contains('\\')
        && !name.split('/').any(|p| p == ".." || p.contains(':'))
}
fn root_for(name: &str) -> Option<String> {
    if name.starts_with("shaders/") {
        return Some(String::new());
    }
    let (first, rest) = name.split_once('/')?;
    if !first.is_empty() && rest.starts_with("shaders/") {
        Some(format!("{first}/"))
    } else {
        None
    }
}
fn archive_root<R: Read + io::Seek>(zip: &mut ZipArchive<R>) -> Result<Option<String>> {
    let mut nested = None;
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i)?;
        let name = entry.name()?.into_owned();
        if !safe(&name) {
            return Err(yc_locale::t!("packaging.shaderpack_unsafe_zip_path", path = name).into());
        }
        if let Some(root) = root_for(&name) {
            if root.is_empty() {
                return Ok(Some(root));
            }
            if nested.is_none() {
                nested = Some(root);
            }
        }
    }
    Ok(nested)
}
pub fn is_shader_pack(source: &ShaderPackSource) -> Result<bool> {
    match source {
        ShaderPackSource::Folder(path) => Ok(path.join("shaders").is_dir()),
        ShaderPackSource::Zip(path) => {
            Ok(archive_root(&mut ZipArchive::new(File::open(path)?)?)?.is_some())
        }
    }
}
/// 入力元の名前を表示名として保ち、ZIP またはフォルダを解析する。
pub fn analyze_source(source: &ShaderPackSource) -> Result<Analysis> {
    let path = match source {
        ShaderPackSource::Zip(path) => return analyze_zip(path),
        ShaderPackSource::Folder(path) => path,
    };
    if !path.join("shaders").is_dir() {
        return Err(yc_locale::t!("packaging.shaderpack_directory_missing").into());
    }
    let lang_dir = path.join("shaders/lang");
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut warnings = Vec::new();
    match fs::read_dir(lang_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let filename = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| yc_locale::t!("packaging.shaderpack_non_utf8_filename"))?;
                if !filename.to_ascii_lowercase().ends_with(".lang") {
                    continue;
                }
                let locale = filename[..filename.len() - 5].to_lowercase();
                if !seen.insert(locale.clone()) {
                    warnings.push(yc_locale::t!(
                        "packaging.shaderpack_duplicate_locale",
                        locale = locale
                    ));
                }
                files.push(LangFile {
                    path: format!("shaders/lang/{filename}"),
                    filename,
                    locale,
                });
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(Analysis {
        root: String::new(),
        files,
        warnings,
        display_name: path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    })
}
pub fn analyze_zip(path: &Path) -> Result<Analysis> {
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let root = archive_root(&mut zip)?
        .ok_or_else(|| yc_locale::t!("packaging.shaderpack_directory_missing"))?;
    let prefix = format!("{root}shaders/lang/");
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut warnings = Vec::new();
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i)?;
        let name = entry.name()?.into_owned();
        if !safe(&name) {
            return Err(yc_locale::t!("packaging.shaderpack_unsafe_zip_path", path = name).into());
        }
        if let Some(filename) = name.strip_prefix(&prefix)
            && !filename.contains('/')
            && filename.to_ascii_lowercase().ends_with(".lang")
        {
            let locale = filename[..filename.len() - 5].to_lowercase();
            if !seen.insert(locale.clone()) {
                warnings.push(yc_locale::t!(
                    "packaging.shaderpack_duplicate_locale",
                    locale = locale
                ));
            }
            files.push(LangFile {
                path: name.clone(),
                filename: filename.to_owned(),
                locale,
            });
        }
    }
    Ok(Analysis {
        root,
        files,
        display_name: path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        warnings,
    })
}
/// Iris と同じく UTF-8 Reader の Properties として解釈する。
pub fn parse_lang(bytes: &[u8]) -> ParsedLang {
    let text = String::from_utf8_lossy(bytes);
    let mut warnings = Vec::new();
    if std::str::from_utf8(bytes).is_err() {
        warnings.push(LangParseWarning::InvalidUtf8);
    }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if yc_language::java_properties_has_invalid_unicode(&normalized) {
        warnings.push(LangParseWarning::InvalidUnicodeEscape);
    }
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut raw_keys = HashMap::new();
    for (key, raw_key, value) in yc_language::parse_java_properties_with_raw_keys(&normalized) {
        if let Some(existing) = entries.iter_mut().find(|(k, _)| k == &key) {
            warnings.push(LangParseWarning::DuplicateKey { key: key.clone() });
            existing.1 = value;
        } else {
            entries.push((key.clone(), value));
        }
        if let Some(raw_key) = raw_key {
            raw_keys.insert(key, raw_key);
        } else {
            raw_keys.remove(&key);
        }
    }
    ParsedLang {
        entries,
        raw_keys,
        warnings,
    }
}
pub fn write_lang(entries: &[(String, String)]) -> Vec<u8> {
    write_lang_with_raw_keys(entries, &HashMap::new())
}
pub fn write_lang_with_raw_keys(
    entries: &[(String, String)],
    raw_keys: &HashMap<String, String>,
) -> Vec<u8> {
    fn escape_key(s: &str) -> String {
        let mut out = String::new();
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{000c}' => out.push_str("\\f"),
                ' ' => out.push_str("\\ "),
                '=' | ':' | '#' | '!' => {
                    out.push('\\');
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
        out
    }
    fn escape_value(s: &str) -> String {
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                ' ' if i == 0 => out.push_str("\\ "),
                '\t' if i == 0 => out.push_str("\\t"),
                '\u{000c}' if i == 0 => out.push_str("\\f"),
                _ => out.push(c),
            }
        }
        out
    }
    let mut out = String::new();
    for (key, value) in entries {
        if let Some(raw_key) = raw_keys.get(key) {
            out.push_str(raw_key);
        } else {
            out.push_str(&escape_key(key));
        }
        out.push('=');
        out.push_str(&escape_value(value));
        out.push('\n');
    }
    out.into_bytes()
}
pub fn write_lang_file(path: &Path, entries: &[(String, String)]) -> Result<()> {
    fs::write(path, write_lang(entries))?;
    Ok(())
}
pub fn japanese_filename(source: &str, target_locale: &str) -> String {
    let locale = yc_language::normalize_locale(target_locale);
    let stem = source
        .rsplit_once('.')
        .filter(|(_, extension)| extension.eq_ignore_ascii_case("lang"))
        .map(|(stem, _)| stem)
        .unwrap_or("");
    let upper_region = stem.rsplit_once('_').is_some_and(|(_, region)| {
        region.len() == 2 && region.bytes().all(|c| c.is_ascii_uppercase())
    });
    let locale = locale
        .split_once('_')
        .map(|(language, region)| {
            if upper_region {
                format!(
                    "{}_{}",
                    language.to_ascii_lowercase(),
                    region.to_ascii_uppercase()
                )
            } else {
                locale.clone()
            }
        })
        .unwrap_or(locale);
    format!("{locale}.lang")
}
fn atomic_zip<F>(output: &Path, f: F) -> Result<()>
where
    F: FnOnce(&mut File) -> Result<()>,
{
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    f(temp.as_file_mut())?;
    temp.persist(output)?;
    Ok(())
}
pub fn write_translated_zip(
    source: &Path,
    output: &Path,
    source_filename: &str,
    target_locale: &str,
    entries: &[(String, String)],
) -> Result<()> {
    write_translated_zip_with_extra_files(
        source,
        output,
        source_filename,
        target_locale,
        entries,
        &[],
    )
}

/// 翻訳済み ZIP を書き出し、`extra_files` を ZIP ルートに追加する。
///
/// PLaMo のライセンス通知をここで渡す。シェーダーパックの ZIP はそのまま配布されるため、
/// 通知をルートに置く必要がある。
pub fn write_translated_zip_with_extra_files(
    source: &Path,
    output: &Path,
    source_filename: &str,
    target_locale: &str,
    entries: &[(String, String)],
    extra_files: &[(String, Vec<u8>)],
) -> Result<()> {
    let mut zip = ZipArchive::new(File::open(source)?)?;
    let root = archive_root(&mut zip)?
        .ok_or_else(|| yc_locale::t!("packaging.shaderpack_directory_missing"))?;
    let prefix = format!("{root}shaders/lang/");
    let source_lang_path = format!("{prefix}{source_filename}");
    let target_locale = yc_language::normalize_locale(target_locale);
    let mut target_paths = Vec::new();
    let mut english_path = None;
    for index in 0..zip.len() {
        if let Ok(entry) = zip.by_index_raw(index)
            && let Ok(name) = entry.name()
            && let Some(file_name) = name.strip_prefix(&prefix)
        {
            let Some((locale, extension)) = file_name.rsplit_once('.') else {
                continue;
            };
            if !extension.eq_ignore_ascii_case("lang") {
                continue;
            }
            let normalized_locale = yc_language::normalize_locale(locale);
            if normalized_locale == target_locale {
                target_paths.push(name.to_string());
            }
            if normalized_locale == "en_us" && english_path.is_none() {
                english_path = Some(name.to_string());
            }
        }
    }
    let target_path = if target_locale == yc_language::DEFAULT_TARGET_LOCALE {
        format!(
            "{prefix}{}",
            japanese_filename(source_filename, &target_locale)
        )
    } else {
        target_paths.first().cloned().unwrap_or_else(|| {
            format!(
                "{prefix}{}",
                japanese_filename(source_filename, &target_locale)
            )
        })
    };
    let baseline_path = if target_locale == "en_us" {
        target_paths
            .first()
            .or(english_path.as_ref())
            .map(String::as_str)
            .unwrap_or(&source_lang_path)
    } else {
        &source_lang_path
    };
    let raw_keys = raw_keys_from_archive(&mut zip, baseline_path)?;
    let output_entries = if target_locale == "en_us" {
        let mut base = if let Some(path) = target_paths.first().or(english_path.as_ref()) {
            let mut entry = zip.by_name(path)?;
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            parse_lang(&bytes).entries
        } else {
            Vec::new()
        };
        for (key, value) in entries {
            if value.trim().is_empty() {
                continue;
            }
            if let Some((_, existing)) = base.iter_mut().find(|(existing, _)| existing == key) {
                *existing = value.clone();
            } else {
                base.push((key.clone(), value.clone()));
            }
        }
        base
    } else {
        entries.to_vec()
    };
    let mut replaced_paths = target_paths.clone();
    if target_locale == "en_us"
        && let Some(path) = &english_path
        && !replaced_paths.contains(path)
    {
        replaced_paths.push(path.clone());
    }
    atomic_zip(output, |file| {
        let mut writer = ZipWriter::new(file);
        let mut has_shaders_directory = false;
        let mut has_lang_directory = false;
        for i in 0..zip.len() {
            let entry = zip.by_index_raw(i)?;
            let name = entry.name()?.into_owned();
            if !safe(&name) {
                return Err(
                    yc_locale::t!("packaging.shaderpack_unsafe_zip_path", path = name).into(),
                );
            }
            has_shaders_directory |= name == format!("{root}shaders/");
            has_lang_directory |= name == prefix;
            if replaced_paths.iter().any(|path| path == &name) {
                continue;
            }
            writer.raw_copy_file(entry)?;
        }
        if !root.is_empty() {
            if !has_shaders_directory {
                writer.add_directory(format!("{root}shaders/"), SimpleFileOptions::default())?;
            }
            if !has_lang_directory {
                writer.add_directory(prefix.clone(), SimpleFileOptions::default())?;
            }
        }
        writer.start_file(&target_path, SimpleFileOptions::default())?;
        writer.write_all(&write_lang_with_raw_keys(&output_entries, &raw_keys))?;
        for (path, bytes) in extra_files {
            if !safe(path) {
                return Err(
                    yc_locale::t!("packaging.shaderpack_unsafe_zip_path", path = path).into(),
                );
            }
            writer.start_file(path, SimpleFileOptions::default())?;
            writer.write_all(bytes)?;
        }
        writer.finish()?;
        Ok(())
    })
}

/// ZIPまたはフォルダ内の言語ファイルからPropertiesキーと生キーの対応を読む。
/// ファイルが見つからない場合は空の対応を返し、書き出し側で従来のキーエスケープを使う。
pub fn raw_keys_for_file(source: &Path, path: &str) -> Result<HashMap<String, String>> {
    if !safe(path) {
        return Err(yc_locale::t!("packaging.shaderpack_unsafe_language_path", path = path).into());
    }
    if source.is_dir() {
        return match fs::read(source.join(path)) {
            Ok(bytes) => Ok(parse_lang(&bytes).raw_keys),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(error) => Err(error.into()),
        };
    }
    let mut zip = ZipArchive::new(File::open(source)?)?;
    raw_keys_from_archive(&mut zip, path)
}

fn raw_keys_from_archive<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    path: &str,
) -> Result<HashMap<String, String>> {
    let mut found = false;
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index)?;
        if entry.name()?.as_ref() == path {
            found = true;
            break;
        }
    }
    if !found {
        return Ok(HashMap::new());
    }
    let mut entry = zip.by_name(path)?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    Ok(parse_lang(&bytes).raw_keys)
}
pub fn pack_folder(folder: &Path, output: &Path) -> Result<()> {
    fn add(writer: &mut ZipWriter<&mut File>, base: &Path, path: &Path) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            let ty = entry.file_type()?;
            if ty.is_symlink() {
                return Err(yc_locale::t!("packaging.shaderpack_symlink_unsupported").into());
            }
            let name = path
                .strip_prefix(base)?
                .to_str()
                .ok_or_else(|| yc_locale::t!("packaging.shaderpack_non_utf8_path"))?
                .replace('\\', "/");
            if !safe(&name) {
                return Err(yc_locale::t!("packaging.shaderpack_unsafe_path", path = name).into());
            }
            if ty.is_dir() {
                writer.add_directory(format!("{name}/"), SimpleFileOptions::default())?;
                add(writer, base, &path)?;
            } else if ty.is_file() {
                writer.start_file(name, SimpleFileOptions::default())?;
                io::copy(&mut File::open(path)?, writer)?;
            }
        }
        Ok(())
    }
    if !folder.join("shaders").is_dir() {
        return Err(yc_locale::t!("packaging.shaderpack_directory_missing").into());
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if fs::canonicalize(parent)?.starts_with(fs::canonicalize(folder)?) {
        return Err(yc_locale::t!("packaging.shaderpack_output_outside_source").into());
    }
    atomic_zip(output, |file| {
        let mut writer = ZipWriter::new(file);
        add(&mut writer, folder, folder)?;
        writer.finish()?;
        Ok(())
    })
}
#[cfg(test)]
mod tests;
