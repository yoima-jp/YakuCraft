//! シェーダーパックを検出し、言語ファイルと ZIP を読み書きする。
//! `analyze_source` で ZIP またはフォルダを解析し、`parse_lang` で言語を読み込む。
//! フォルダは `pack_folder` で ZIP に変換し、`write_translated_zip` で日本語化する。

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
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
    pub warnings: Vec<String>,
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
            return Err(format!("unsafe ZIP path: {name}").into());
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
        return Err("shaders directory not found".into());
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
                    .map_err(|_| "non-UTF-8 filename")?;
                if !filename.to_ascii_lowercase().ends_with(".lang") {
                    continue;
                }
                let locale = filename[..filename.len() - 5].to_lowercase();
                if !seen.insert(locale.clone()) {
                    warnings.push(format!("duplicate locale: {locale}"));
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
    let root = archive_root(&mut zip)?.ok_or("shaders directory not found")?;
    let prefix = format!("{root}shaders/lang/");
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut warnings = Vec::new();
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i)?;
        let name = entry.name()?.into_owned();
        if !safe(&name) {
            return Err(format!("unsafe ZIP path: {name}").into());
        }
        if let Some(filename) = name.strip_prefix(&prefix)
            && !filename.contains('/')
            && filename.to_ascii_lowercase().ends_with(".lang")
        {
            let locale = filename[..filename.len() - 5].to_lowercase();
            if !seen.insert(locale.clone()) {
                warnings.push(format!("duplicate locale: {locale}"));
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
        warnings.push("invalid UTF-8".into());
    }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if yc_language::java_properties_has_invalid_unicode(&normalized) {
        warnings.push("invalid Unicode escape".into());
    }
    let mut entries: Vec<(String, String)> = Vec::new();
    for (key, value) in yc_language::parse_java_properties(&normalized) {
        if let Some(existing) = entries.iter_mut().find(|(k, _)| k == &key) {
            warnings.push(format!("duplicate key: {key}"));
            existing.1 = value;
        } else {
            entries.push((key, value));
        }
    }
    ParsedLang { entries, warnings }
}
pub fn write_lang(entries: &[(String, String)]) -> Vec<u8> {
    fn escape(s: &str, key: bool) -> String {
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{000c}' => out.push_str("\\f"),
                ' ' if key || i == 0 => out.push_str("\\ "),
                '=' | ':' | '#' | '!' if key => {
                    out.push('\\');
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
        out
    }
    let mut out = String::new();
    for (key, value) in entries {
        out.push_str(&escape(key, true));
        out.push('=');
        out.push_str(&escape(value, false));
        out.push('\n');
    }
    out.into_bytes()
}
pub fn write_lang_file(path: &Path, entries: &[(String, String)]) -> Result<()> {
    fs::write(path, write_lang(entries))?;
    Ok(())
}
pub fn japanese_filename(source: &str) -> &'static str {
    let stem = source
        .rsplit_once('.')
        .filter(|(_, extension)| extension.eq_ignore_ascii_case("lang"))
        .map(|(stem, _)| stem)
        .unwrap_or("");
    let Some((language, region)) = stem.split_once('_') else {
        return "ja_jp.lang";
    };
    if language.len() == 2
        && region.len() == 2
        && language.bytes().all(|c| c.is_ascii_lowercase())
        && region.bytes().all(|c| c.is_ascii_uppercase())
    {
        "ja_JP.lang"
    } else {
        "ja_jp.lang"
    }
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
    entries: &[(String, String)],
) -> Result<()> {
    let mut zip = ZipArchive::new(File::open(source)?)?;
    let root = archive_root(&mut zip)?.ok_or("shaders directory not found")?;
    let prefix = format!("{root}shaders/lang/");
    atomic_zip(output, |file| {
        let mut writer = ZipWriter::new(file);
        for i in 0..zip.len() {
            let entry = zip.by_index_raw(i)?;
            let name = entry.name()?.into_owned();
            if !safe(&name) {
                return Err(format!("unsafe ZIP path: {name}").into());
            }
            if name
                .strip_prefix(&prefix)
                .is_some_and(|n| n.eq_ignore_ascii_case("ja_jp.lang"))
            {
                continue;
            }
            writer.raw_copy_file(entry)?;
        }
        writer.start_file(
            format!("{prefix}{}", japanese_filename(source_filename)),
            SimpleFileOptions::default(),
        )?;
        writer.write_all(&write_lang(entries))?;
        writer.finish()?;
        Ok(())
    })
}
pub fn pack_folder(folder: &Path, output: &Path) -> Result<()> {
    fn add(writer: &mut ZipWriter<&mut File>, base: &Path, path: &Path) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            let ty = entry.file_type()?;
            if ty.is_symlink() {
                return Err("symbolic links are not supported".into());
            }
            let name = path
                .strip_prefix(base)?
                .to_str()
                .ok_or("non-UTF-8 path")?
                .replace('\\', "/");
            if !safe(&name) {
                return Err(format!("unsafe path: {name}").into());
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
        return Err("shaders directory not found".into());
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if fs::canonicalize(parent)?.starts_with(fs::canonicalize(folder)?) {
        return Err("output must be outside the source folder".into());
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
