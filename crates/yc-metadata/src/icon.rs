use std::io::Cursor;

use serde_json::Value;

use crate::{ArchiveLookup, parse_json_relaxed, toml_metadata};

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
// アイコンは小さな UI 表示用アセットなので、圧縮データと展開後の画素に上限を設け、
// 巨大画像による検証中・返却後のメモリ消費を抑える。
const MAX_ICON_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IconMediaType {
    Png,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModIcon {
    pub archive_path: String,
    pub bytes: Vec<u8>,
    pub media_type: IconMediaType,
}

/// Finds the first usable PNG icon declared by supported mod metadata.
pub fn find_mod_icon<A: ArchiveLookup + ?Sized>(
    archive: &A,
    mod_id: Option<&str>,
) -> Option<ModIcon> {
    read_metadata_icon(archive, "fabric.mod.json", mod_id, json_fabric_icon)
        .or_else(|| read_metadata_icon(archive, "quilt.mod.json", mod_id, json_quilt_icon))
        .or_else(|| {
            read_metadata_icon(
                archive,
                "META-INF/mods.toml",
                mod_id,
                toml_metadata::find_icon_path,
            )
        })
        .or_else(|| {
            read_metadata_icon(
                archive,
                "META-INF/neoforge.mods.toml",
                mod_id,
                toml_metadata::find_icon_path,
            )
        })
        .or_else(|| read_metadata_icon(archive, "mcmod.info", mod_id, json_legacy_icon))
        .or_else(|| read_icon(archive, "pack.png"))
}

fn read_metadata_icon<A: ArchiveLookup + ?Sized>(
    archive: &A,
    metadata_path: &str,
    mod_id: Option<&str>,
    extract_path: fn(&[u8], Option<&str>) -> Option<String>,
) -> Option<ModIcon> {
    let metadata = archive.read(metadata_path)?;
    let icon_path = extract_path(&metadata, mod_id)?;
    read_icon(archive, &icon_path)
}

fn read_icon<A: ArchiveLookup + ?Sized>(archive: &A, path: &str) -> Option<ModIcon> {
    let archive_path = normalize_icon_path(path)?;
    let bytes = archive.read(&archive_path)?;
    if bytes.len() > MAX_ICON_BYTES
        || !bytes.starts_with(PNG_SIGNATURE)
        || !is_decodable_png(&bytes)
    {
        return None;
    }

    Some(ModIcon {
        archive_path,
        bytes,
        media_type: IconMediaType::Png,
    })
}

fn is_decodable_png(bytes: &[u8]) -> bool {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_limits(png::Limits {
        bytes: MAX_ICON_BYTES,
    });
    let Ok(mut reader) = decoder.read_info() else {
        return false;
    };
    let Some(output_size) = reader.output_buffer_size() else {
        return false;
    };
    if output_size > MAX_ICON_BYTES {
        return false;
    }

    let mut decoded = vec![0; output_size];
    reader.next_frame(&mut decoded).is_ok() && reader.finish().is_ok()
}

fn normalize_icon_path(path: &str) -> Option<String> {
    // Loader metadata commonly prefixes icon paths with `/`, while ArchiveLookup names
    // are relative to the archive root. Strip those prefixes before applying traversal checks.
    let path = path.trim_start_matches('/');
    if path.is_empty() || path.contains('\0') || path.contains('\\') {
        return None;
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return None;
    }

    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => return None,
            value => components.push(value),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

fn json_fabric_icon(bytes: &[u8], _mod_id: Option<&str>) -> Option<String> {
    let root = parse_json_relaxed(bytes).ok()?;
    icon_reference(root.get("icon")?)
}

fn json_quilt_icon(bytes: &[u8], _mod_id: Option<&str>) -> Option<String> {
    let root = parse_json_relaxed(bytes).ok()?;
    let icon = root.get("quilt_loader")?.get("metadata")?.get("icon")?;
    icon_reference(icon)
}

fn json_legacy_icon(bytes: &[u8], mod_id: Option<&str>) -> Option<String> {
    let root = parse_json_relaxed(bytes).ok()?;
    let records = root
        .as_array()
        .or_else(|| root.get("modList").and_then(Value::as_array));
    let selected = records.and_then(|records| {
        mod_id
            .and_then(|mod_id| {
                records
                    .iter()
                    .find(|record| legacy_mod_id(record) == Some(mod_id))
                    .and_then(|record| string_path(record.get("logoFile")))
            })
            .or_else(|| {
                records
                    .iter()
                    .find_map(|record| string_path(record.get("logoFile")))
            })
    });

    selected.or_else(|| string_path(root.get("logoFile")))
}

fn icon_reference(value: &Value) -> Option<String> {
    match value {
        Value::String(_) => string_path(Some(value)),
        Value::Object(entries) => {
            let sized_paths: Vec<(u32, &str)> = entries
                .iter()
                .filter_map(|(size, path)| Some((size.parse().ok()?, path.as_str()?)))
                .filter(|(_, path)| !path.trim().is_empty())
                .collect();

            sized_paths
                .iter()
                .filter(|(size, _)| *size >= 64)
                .min_by_key(|(size, _)| *size)
                .or_else(|| sized_paths.iter().max_by_key(|(size, _)| *size))
                .map(|(_, path)| (*path).to_owned())
        }
        _ => None,
    }
}

fn legacy_mod_id(record: &Value) -> Option<&str> {
    record
        .get("modid")
        .or_else(|| record.get("modId"))
        .and_then(Value::as_str)
}

fn string_path(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(str::to_owned)
}
